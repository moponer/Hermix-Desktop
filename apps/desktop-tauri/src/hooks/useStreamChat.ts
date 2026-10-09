import { useState, useCallback, useRef, useEffect, useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AcuiDirective } from "../acui/types";
import type { ApprovalRequestItem, SubAgentEventPayload } from "../types";
import { createPhaseController } from "./useStreamPhase";
import { createMessageHandler, type StreamChatSendContext } from "./useStreamParser";
import { createConnector } from "./useWsConnection";
import {
  buildConnectedPayload,
  computeSendDedupHash,
  stripActivityMarkers,
} from "./streamChatUtils";
import { streamChannelName } from "../api";

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
 * (`hermix-interport/src/lib.rs`).
 */
export interface ChatOverrides {
  /** Optional path the agent's read/write tools are restricted to. */
  workspace?: string;
  /** Inline system-prompt block prepended at the top of the agent's prompt. */
  systemPromptOverride?: string;
  /** Pre-resolved tool whitelist. */
  tools?: string[];
  /** Backend `personality` slug or inline blob. */
  personality?: string;
  /** Permission level — "default" (ask on risky) | "full" (no ask). */
  permissionLevel?: "default" | "full";
  /** 模型路由 — 形如 "minimax:MiniMax-M2.7" 的 "provider:model" 串。 */
  model?: string;
  /** Names of skills to enable for this turn. */
  skills?: string[];
  /** L1 域模式 pin："code" = 编码模式；不传/undefined = 自动检测（工作模式）。 */
  domainOverride?: string;
  /** 受聘专家携带的工具组名（registry 标签，如 "office"）。后端把组成员
   *  并进本轮白名单——专家只能加工具，不会撤销用户已启用的集合。 */
  expertToolset?: string;
  /** Free-form overrides for future use; backend ignores unknown keys. */
  /** 多图支持 — binary bytes 直传，不走 base64。 */
  images?: { bytes: number[]; mimeType: string; name?: string }[];
  image?: string;  // legacy single-image base64 (backward compat)
  imageMimeType?: string;  // mime type for legacy single image
  /** 非图片附件（拖放/粘贴/选择器）。WebView2 拿不到 File 的磁盘路径，
   *  所以字节直传（镜像 images 通道）；服务端落盘 <hermix_home>/attachments/
   *  后把真实路径交给 agent（read_file / read_pdf 读取）。 */
  attachedFiles?: { name: string; mimeType?: string; bytes: number[] }[];
  /** 客户端已持有真实磁盘路径的附件（工作台文件树"加入对话"），
   *  直传路径、无需落盘。 */
  attachedFilePaths?: string[];
  /** 每轮"思考开/关"开关。`true`/`false` 显式下发到后端（透传 provider
   *  的 `enable_thinking` 参数）；`undefined` = 不下发，后端保持默认行为。 */
  enableThinking?: boolean;
  /** Extensibility index — backend ignores unknown keys. */
  [key: string]: unknown;
}

/** One prefetch result entry sent over the wire. */
export interface PrefetchResultEntry {
  tool: string;
  args: unknown;
  output: string;
  errored: boolean;
  durationMs: number;
}

export interface StreamCallbacks {
  /** Called on every phase transition. */
  onPhaseChange?: (phase: StreamPhase) => void;
  onChunk?: (text: string) => void;
  onToolStart?: (info: { toolCallId: string; tool: string; content?: string }) => void;
  onToolComplete?: (info: {
    toolCallId: string;
    tool: string;
    content?: string;
    denialKind?: string;
    /** Server-authoritative `ToolResult::is_error`. */
    isError?: boolean;
  }) => void;
  onToolChunk?: (info: {
    toolCallId: string;
    tool: string;
    content: string;
    chunkIndex: number;
    chunkTotal: number;
  }) => void;
  onFileChange?: (info: {
    toolCallId: string;
    filePath: string;
    changeType: "created" | "modified" | "deleted" | "renamed";
    fileSummary?: string;
  }) => void;
  onMemoryUpdate?: (info: {
    action: "write" | "update" | "delete" | "summarize" | "cleanup" | "snapshot";
    memoryType: "fact" | "style" | "summary" | "tool" | "snapshot" | "reflection";
    key: string;
    summary: string;
    count?: number;
  }) => void;
  onMemoryRecall?: (info: {
    trigger: "first_turn" | "interval" | "drift";
    count: number;
    totalChars?: number;
    previewKeys: string[];
    turnNumber: number;
  }) => void;
  onSubAgentEvent?: (info: SubAgentEventPayload) => void;
  onTodoChange?: (change:
    | { kind: "snapshot"; sessionId: string; items: TodoSnapshotItem[] }
    | { kind: "group_started"; sessionId: string; groupId: string; groupTitle: string | null; messageId: string | null }
    | { kind: "upsert"; sessionId: string; item: TodoSnapshotItem }
    | { kind: "remove"; sessionId: string; id: string }
  ) => void;
  onApprovalRequest?: (req: {
    requestId: string;
    tool: string;
    args: unknown;
    riskLevel?: string;
    expiresAt?: number | null;
    reason?: string;
    sessionId: string;
  }) => Promise<{ decision: "approved" | "denied" | "expired"; remember?: boolean; note?: string }>;
  /** Server coalesced N parallel approvals into one card. The
   *  settlement promise carries the single decision (no per-item
   *  granularity at the wire level) plus an optional
   *  `remember=true` that translates to wire decision `always`. */
onApprovalBatchRequest?: (req: {
    batchId: string;
    items: ApprovalRequestItem[];
    sessionId: string;
  }) => Promise<{ decision: "approved" | "denied" | "expired"; remember?: boolean; note?: string }>;
  onClarifyRequest?: (req: {
    requestId: string;
    question: string;
    choices: string[];
    sessionId: string;
  }) => Promise<{ answer: string }>;
  onDone?: (fullText: string, stopReason?: string, durationMs?: number) => void;
  onError?: (error: string, detail?: string, kind?: string) => void;
  /**
   * Fires when the WS reconnection budget is exhausted and the
   * connector has nothing left to fall back to. The caller should
   * surface "网络中断" toast / banner immediately and (optionally)
   * kick off a REST polling fallback. Mirror of
   * `StreamChatSendContext.onConnectionLost`.
   */
  onConnectionLost?: () => void;
  /**
   * Fires when a previously-lost connection transitions back to
   * OPEN. Lets the caller clear the "网络中断" affordance.
   */
  onConnectionRestored?: () => void;
  /**
   * P5 L3 seq 对账（设计 §3.5 / §6.3 P7）：WS 连接丢失后
   * （bridge 死 / 监听器注册失败），connector 调
   * `subscribeSessionSnapshot(sessionId, lastLocalSeq)` 对账，
   * 把结果交给此回调——组件据此刷新本地会话状态。
   * `fullSnapshot=true`（落后）→ 用 `messages` 整体替换本地会话
   * 状态；`fullSnapshot=false`（已追上空增量）→ 仅刷新本地 seq
   * 锚（`lastSeq`），不替换消息。`lastSeq` 始终更新本地水位，
   * 供下次重连作 since_seq。
   * 与 `onConnectionRestored` 区分：那个是「连接回来了」的事件
   * 回调（无数据）；这个是「连接丢失后拉了一次对账」的数据回调。
   */
  onReconciled?: (snapshot: {
    sessionId: string;
    messages: unknown[];
    lastSeq: number;
    fullSnapshot: boolean;
  }) => void;
  onReasoningChunk?: (piece: string) => void;
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
  onPrefetch?: (info: {
    intent: string;
    confidence: number;
    results: PrefetchResultEntry[];
  }) => void;
  onAcuiDirective?: (directive: AcuiDirective) => void;
  onCancel?: () => void;
}

export function useStreamChat(activeSessionId: string | null) {
  // Per-session streaming state. We track the *session id* (not just a
  // boolean) so the UI can scope the "正在思考…" indicator and the
  // composer-disabled state to the specific session that's mid-stream.
  const [streamingSessionId, setStreamingSessionId] = useState<string | null>(null);
  const streamingSessionIdRef = useRef<string | null>(null);
  const [streamingStatus, setStreamingStatus] = useState<string | null>(null);
  const [compactionStatus, setCompactionStatus] =
    useState<import("../components/CompactionStatusBanner").CompactionStatus | null>(null);
  const [contextUsage, setContextUsage] =
    useState<{ usedTokens: number; maxTokens: number; thresholdTokens: number; percent: number } | null>(null);
  const [usageSnapshot, setUsageSnapshot] =
    useState<{
      promptTokens: number;
      completionTokens: number;
      cacheReadTokens: number;
      cacheCreationTokens: number;
      reasoningTokens: number;
    } | null>(null);
  const [lastActivityAt, setLastActivityAt] = useState<number | null>(null);
  // stdio bridge: wsRef now holds the listener-unlisten function (the
  // bridge never "closes" — the channel disappears only when the
  // server stops, which the watchdog handles out-of-band). We keep
  // the `.close()`-shape call sites so the rest of the file does
  // not need to know about the transport.
  const wsRef = useRef<(() => void) | null>(null);
  // type-B observability + direct-send: send a raw text frame on the
  // live `stream:<sessionId>` channel via the stdio bridge. Returns
  // false (no-op) if the channel is not currently subscribed, so
  // callers can distinguish "sent" from "dropped". Used by the
  // approval flow so the click handler can emit `approval_response`
  // directly AND so we can route diagnostics to the server log.
  const wsSendRaw = useCallback((text: string): boolean => {
    const unlisten = wsRef.current;
    if (!unlisten) return false;
    try {
      // Channel is implicit in the global "send raw on the
      // currently-streaming session" rule — caller is App.tsx's
      // approval/clarity path, which only fires while a stream is
      // active, and `streamingSessionIdRef` tells us which channel.
      const sid = streamingSessionIdRef.current;
      if (!sid) return false;
      void invoke<void>("backend_channel_send", {
        channel: `stream:${sid}`,
        payload: text,
      });
      return true;
    } catch {
      return false;
    }
  }, []);
  const pendingResolveRef = useRef<((value: string) => void) | null>(null);
  const pendingRejectRef = useRef<((error: Error) => void) | null>(null);
  const pendingCallbacksRef = useRef<StreamCallbacks | null>(null);
  const disarmTimeoutsRef = useRef<(() => void) | null>(null);
  // (storage-analyzer stale_stream bug fix): while a tool is
  // executing on the server (between `tool_start` and `tool_complete`),
  // suspend the STALE_STREAM_TIMEOUT countdown.
  const toolRunningRef = useRef(false);
  const sendSettledRef = useRef(false);
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
  // Client-side dedup: track last sent (hash, timestamp).
  const lastSendRef = useRef<{ hash: string; ts: number } | null>(null);
  // Per-send isolation via a monotonically increasing ID.
  const activeSendIdRef = useRef(0);
  const activeSessionIdRef = useRef<string | null>(activeSessionId);
  useEffect(() => { activeSessionIdRef.current = activeSessionId; }, [activeSessionId]);
  const wsSessionIdRef = useRef<string | null>(null);
  // shared per-send state via refs so that WS reconnection
  // (doConnect retry) shares the same `done` / `sentToServer` flags.
  const doneRef = useRef(false);
  const sentToServerRef = useRef(false);
  // Per-send wall-clock start.
  const sendStartedAtRef = useRef<number | null>(null);
  // Holds the per-send `callbacks.onPrefetch` closure.
  const onPrefetchRef = useRef<((info: {
    intent: string;
    confidence: number;
    results: PrefetchResultEntry[];
  }) => void) | undefined>(undefined);

  // Defensive cleanup: if the component unmounts while a stream is
  // still in flight, close the WS.
  useEffect(() => {
    return () => {
      console.log("[WS-CLOSE] site=unmount (组件卸载时仍有在途流，主动关闭 WS)");
      wsRef.current?.();
      wsRef.current = null;
    };
  }, []);

  // (P0-7 / STABILITY-PLAN §3.2) — listen for the stdio bridge death event
  // emitted by `stdio_bridge::mark_dead_and_notify` in the Tauri host.
  // Without this hook, an in-flight stream survives `stdout closed` for as
  // long as `STALE_STREAM_TIMEOUT_MS` (300s). The server emits this event
  // on the same generation as the active reader, so we only settle when the
  // bridge death *actually* belongs to the send we are running (active
  // generation match is implied by "the only possible listener is this
  // current effect, mounted at most once").
  useEffect(() => {
    if (typeof window === "undefined") return;
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    // Dynamic import: tauri listene API is browser-side at runtime.
    void import("@tauri-apps/api/event")
      .then(({ listen }) => {
        if (cancelled) return;
        listen<{ reason: string; generation: number; current_generation: number }>(
          "hermix:stream-bridge-dead",
          (evt) => {
            // Settle the in-flight stream IMMEDIATELY — the bridge wrote no
            // more frames; nothing left to wait for. Skip when no stream is
            // currently active to avoid stale-event spam during app shutdown.
            const settle = settleRef.current;
            if (!settle) return;
            console.warn(
              `[WS-BRIDGE-DEAD] site=stdio_bridge reason=${evt.payload?.reason ?? "unknown"} gen=${evt.payload?.generation}/${evt.payload?.current_generation}`,
            );
            settle(
              "",
              "连接中断：后端进程已退出，请重新发送。",
              "bridge_dead",
              "error",
              evt.payload?.reason,
              false,
              "bridge_dead",
            );
          },
        ).then((u) => {
          if (cancelled) {
            u();
            return;
          }
          unlisten = u;
        });
      })
      .catch((e) => console.error("[WS-BRIDGE-DEAD] listener attach failed", e));
    return () => {
      cancelled = true;
      unlisten?.();
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
        // Mark the start of this assistant turn.
        sendStartedAtRef.current = Date.now();
        setLastActivityAt(Date.now());

        // ── Client-side message dedup (runs BEFORE setStreamingSessionId) ──
        // A duplicate must resolve("") cleanly instead of `return`-ing from
        // inside this Promise executor.
        const DEDUP_WINDOW_MS = 15_000;
        const contentHash = computeSendDedupHash(sessionId, content);
        const now = Date.now();
        if (
          lastSendRef.current &&
          lastSendRef.current.hash === contentHash &&
          now - lastSendRef.current.ts < DEDUP_WINDOW_MS &&
          streamingSessionIdRef.current === sessionId
        ) {
          resolve("");
          return;
        }
        lastSendRef.current = { hash: contentHash, ts: now };

        // Increment the active send ID so any stale WS event handlers from a
        // previous `send()` call become no-ops.
        const mySendId = ++activeSendIdRef.current;

        streamingSessionIdRef.current = sessionId;
        setStreamingSessionId(sessionId);
        doneRef.current = false;
        sentToServerRef.current = false;
        let stopReason: string | undefined;

        // ── Shared per-send context ─────────────────────────────
        // The original `send` closure kept all of this as bare `let`s and
        // refs; we hoist them onto one mutable object so the (now extracted)
        // `createMessageHandler` (stream parser) and `createConnector` (WS
        // connection) can read/write the same state without re-writing the
        // closure shape. Behaviour is identical to the pre-refactor code.
        const ctx: StreamChatSendContext = {
          sessionId,
          content,
          overrides,
          callbacks,
          mySendId,
          activeSendIdRef,
          wsRef,
          wsSessionIdRef,
          doneRef,
          sentToServerRef,
          toolRunningRef,
          activeSessionIdRef,
          onPrefetchRef,
          streamingSessionIdRef,
          // WS lifecycle hooks forwarded from `callbacks`. Tracked here so
          // the WS connector can fire them without going through the
          // messaging handler.
          onConnectionLost: callbacks.onConnectionLost,
          onConnectionRestored: callbacks.onConnectionRestored,
          setStreamingSessionId,
          setStreamingStatus,
          setLastActivityAt,
          setContextUsage,
          setUsageSnapshot,
          setCompactionStatus,
          fullText: "",
          phase: "connecting",
          firstChunkReceived: false,
          hasContent: false,
          hasAnyStreamEvent: false,
          approvalPending: false,
          idleTimeout: null,
          slowHintTimer: null,
          staleStreamTimer: null,
          chunkBuffer: "",
          chunkRaf: null,
          retryCount: 0,
          // P5 L3 seq 对账：本 attachment 的本地 seq 水位初值 0。
          // 首帧带 seq 时由 useStreamParser 更新（max 语义）；重连
          // 对账时作 subscribeSessionSnapshot 的 since_seq。
          lastLocalSeq: 0,
          // Helpers are wired in below (after they're defined).
          updatePhase: () => {},
          getPhase: () => "idle",
          settle: () => {},
          armTimeouts: () => {},
          disarmTimeouts: () => {},
          resetIdleTimeout: () => {},
          resetStaleTimer: () => {},
          resetMidStreamSilenceTimer: () => {},
          scheduleChunkFlush: () => {},
          wsSendRaw,
          dispatchUserMessage: () => {},
        };

        // Phase tracking lives in its own controller now.
        const phaseCtrl = createPhaseController(callbacks.onPhaseChange);
        ctx.getPhase = () => phaseCtrl.getPhase();
        ctx.updatePhase = (next: StreamPhase) => {
          ctx.phase = next;
          phaseCtrl.updatePhase(next);
        };

        // ── Per-send chunk batching ─────────────────────────────
        // Collapse N chunks into one batch per rAF tick.
        const flushChunks = () => {
          ctx.chunkRaf = null;
          const batched = ctx.chunkBuffer;
          ctx.chunkBuffer = "";
          if (batched) callbacks.onChunk?.(batched);
        };
        const scheduleChunkFlush = () => {
          if (ctx.chunkRaf !== null) return;
          ctx.chunkRaf = requestAnimationFrame(flushChunks);
        };
        ctx.scheduleChunkFlush = scheduleChunkFlush;

        // ── Phase tracking timers ───────────────────────────────
        const IDLE_TIMEOUT_MS = 300_000;
        const STALE_STREAM_TIMEOUT_MS = 300_000;
        const TOOL_STALE_CAP_MS = STALE_STREAM_TIMEOUT_MS * 2;
        // (P0-7 / STABILITY-PLAN §3.3) — mid-stream silence watchdog:
        // after at least one chunk has arrived, fire `mid_stream_silence`
        // if no event (chunk/tool/file/etc.) is observed for
        // MID_STREAM_SILENCE_MS — catches the "14% 永久不动" class of hangs
        // where the backend is alive but stuck (deadlock, blocked syscall,
        // provider stalled, etc.) and never sends a terminal event.
        // Tools in flight get the longer TOOL_STALE_CAP_MS window.
        const MID_STREAM_SILENCE_MS = 60_000;
        let midStreamSilenceTimer: ReturnType<typeof setTimeout> | null = null;
        const resetMidStreamSilenceTimer = () => {
          if (midStreamSilenceTimer !== null) clearTimeout(midStreamSilenceTimer);
          if (!ctx.firstChunkReceived) return;
          const cap = ctx.toolRunningRef.current
            ? TOOL_STALE_CAP_MS
            : MID_STREAM_SILENCE_MS;
          midStreamSilenceTimer = setTimeout(() => {
            if (!ctx.doneRef.current) {
              console.warn(
                `[WS-STALL] site=mid_stream_silence tool_running=${ctx.toolRunningRef.current} elapsed_ms=${cap}`,
              );
              ctx.settle(
                "",
                undefined,
                "mid_stream_silence",
                "timeout",
                `后端 ${Math.round(cap / 1000)}s 内无任何事件`,
              );
            }
          }, cap);
        };
        const resetIdleTimeout = () => {
          if (ctx.idleTimeout !== null) clearTimeout(ctx.idleTimeout);
          ctx.idleTimeout = setTimeout(() => {
            if (!ctx.doneRef.current) {
              ctx.updatePhase("timeout");
              ctx.settle("", undefined, "timeout", "timeout");
            }
          }, IDLE_TIMEOUT_MS);
        };
        const resetStaleTimer = () => {
          if (ctx.staleStreamTimer !== null) clearTimeout(ctx.staleStreamTimer);
          const timeout = ctx.toolRunningRef.current
            ? TOOL_STALE_CAP_MS
            : STALE_STREAM_TIMEOUT_MS;
          ctx.staleStreamTimer = setTimeout(() => {
            if (!ctx.doneRef.current) {
              ctx.settle("", undefined, "stale_stream", "partial");
            }
          }, timeout);
        };
        const armTimeouts = () => {
          resetIdleTimeout();
          resetStaleTimer();
          resetMidStreamSilenceTimer();
        };
        // Expose so the parser can re-arm on every tool/chunk event without
        // dragging a new closure through the message handler.
        ctx.armTimeouts = armTimeouts;
        ctx.resetMidStreamSilenceTimer = resetMidStreamSilenceTimer;

        // ── Dispatch this turn's user message ────────────────────
        // Idempotent by design. Two code paths may call it:
        //   1. the parser's `connected` branch (TCP-era behaviour,
        //      preserved for any transport that still emits it), and
        //   2. the connector, immediately after its listener is
        //      registered.
        //
        // Path 2 exists because the stdio bridge creates one channel
        // per session and keeps it alive across sends, so only the
        // FIRST send on a session ever receives `connected`. Every
        // later send used to wait forever for an event that would
        // never arrive — the message was never dispatched at all and
        // the UI sat on "正在思考…" indefinitely with zero traffic
        // upstream. Whichever path fires first dispatches; the other
        // is a cheap no-op.
        const dispatchUserMessage = () => {
          if (ctx.sentToServerRef.current) return;
          if (ctx.doneRef.current) return;
          const payload = buildConnectedPayload(ctx.content, ctx.overrides);
          // Fire-and-forget: the bridge's invoke round-trip is what
          // would have been a WS `send` in the TCP era. A transient
          // invoke failure is recoverable (the upstream
          // approval/clarity path already has its own retry via
          // `wsSendRaw`).
          void invoke<void>("backend_channel_send", {
            channel: streamChannelName(ctx.sessionId),
            payload: JSON.stringify(payload),
          });
          ctx.sentToServerRef.current = true;
          ctx.updatePhase("streaming");
          armTimeouts();

          // 30s slow-hint timer: if the first chunk hasn't arrived by
          // then, the stream is still alive (no done/error yet).
          ctx.slowHintTimer = setTimeout(() => {
            if (!ctx.firstChunkReceived && !ctx.doneRef.current) ctx.updatePhase("slow");
          }, 30_000);
        };
        ctx.dispatchUserMessage = dispatchUserMessage;
        const disarmTimeouts = () => {
          if (ctx.idleTimeout !== null) {
            clearTimeout(ctx.idleTimeout);
            ctx.idleTimeout = null;
          }
          if (ctx.staleStreamTimer !== null) {
            clearTimeout(ctx.staleStreamTimer);
            ctx.staleStreamTimer = null;
          }
          if (ctx.slowHintTimer !== null) {
            clearTimeout(ctx.slowHintTimer);
            ctx.slowHintTimer = null;
          }
          ctx.toolRunningRef.current = false;
          if (midStreamSilenceTimer !== null) {
            clearTimeout(midStreamSilenceTimer);
            midStreamSilenceTimer = null;
          }
        };
        ctx.disarmTimeouts = disarmTimeouts;
        ctx.resetIdleTimeout = resetIdleTimeout;
        ctx.resetStaleTimer = resetStaleTimer;
        ctx.resetMidStreamSilenceTimer = resetMidStreamSilenceTimer;
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
          if (ctx.doneRef.current) return;
          // Guard against stale WS event handlers.
          if (ctx.activeSendIdRef.current !== ctx.mySendId) return;
          // Turn is truly ending.
          setLastActivityAt(null);
          disarmTimeouts();
          // Drain any chunks that arrived after the last rAF tick.
          if (ctx.chunkRaf !== null) {
            cancelAnimationFrame(ctx.chunkRaf);
            ctx.chunkRaf = null;
          }
          const pending = ctx.chunkBuffer;
          ctx.chunkBuffer = "";
          if (pending) callbacks.onChunk?.(pending);

          ctx.doneRef.current = true;
          // Stream is ending (done/error/cancel) — clear any in-flight
          // compaction banner. Without this, a compression whose terminal
          // snapshot never arrives (cancel, WS drop, backend panic) leaves
          // the "压缩中" spinner stuck forever. Terminal phases
          // (complete/failed) are kept so the user still sees the result.
          setCompactionStatus((prev) =>
            prev &&
            (prev.phase === "starting" ||
              prev.phase === "in_progress_fast" ||
              prev.phase === "in_progress_llm")
              ? null
              : prev,
          );
          if (keepSessionAlive) {
            console.log(`[WS-KEEPALIVE] site=settle reason=${reason ?? "interrupted"} (outcome!=completed, session preserved)`);
          } else {
            streamingSessionIdRef.current = null;
            setStreamingSessionId(null);
            setStreamingStatus(null);
            console.log(`[WS-CLOSE] site=settle reason=${reason ?? "natural-done"}`);
            wsRef.current?.();
            wsRef.current = null;
          }
          if (reason) stopReason = reason;
          // Emit terminal phase
          ctx.updatePhase(phaseHint ?? (err ? "error" : "completed"));

          // Strip leaked status/activity markers from final text.
          const cleanText = stripActivityMarkers(text);

          if (err) {
            callbacks.onError?.(err, detail, kind);
            (pendingRejectRef.current ?? reject)(new Error(err));
          } else {
            const durationMs = sendStartedAtRef.current !== null
              ? Math.max(0, Date.now() - sendStartedAtRef.current)
              : undefined;
            callbacks.onDone?.(cleanText, stopReason, durationMs);
            (pendingResolveRef.current ?? resolve)(cleanText);
          }
          // Clean up per-send refs.
          pendingResolveRef.current = null;
          pendingRejectRef.current = null;
          pendingCallbacksRef.current = null;
          sendStartedAtRef.current = null;
          sendSettledRef.current = true;
          settleRef.current = null;
        };
        ctx.settle = settle;
        settleRef.current = settle;

        // ── Delegate the wire protocol ──────────────────────────
        // `createMessageHandler` owns the full `ws.onmessage` parse/merge
        // logic; `createConnector` owns `new WebSocket(...)` + retry. Both
        // read/write the shared `ctx`.
        const onMessage = createMessageHandler(ctx);
        const connect = createConnector(ctx, onMessage);
        connect();
      });
    },
    []
  );

  /**
   * Stop the in-flight stream.
   *
   * The optional `sessionId` guards against the "two conversations
   * share the same stop button" bug: only settle the stream if the
   * currently-active streaming session matches `sessionId`. This
   * matters because `streamingSessionId` is a SINGLE shared slot at
   * the React top level — without this guard, clicking Stop on
   * ChatView B while ChatView A is the one that actually owns the
   * active stream would still settle A, leaving the user
   * bewildered ("I clicked stop on B, why did A die?").
   *
   * Returns `true` if a stream was actually cancelled, `false` if the
   * call was a no-op (caller is not the active session, or nothing
   * is streaming right now). The Composer uses this to know whether
   * to fall back to its legacy "全局 cancel" behavior when called
   * from contexts that don't have a sessionId handy.
   */
  const cancel = useCallback((sessionId?: string): boolean => {
    const currentStreamId = streamingSessionIdRef.current;
    if (sessionId !== undefined && currentStreamId !== sessionId) {
      // Mismatched session — refuse to cancel. We deliberately do
      // NOT touch `streamingSessionId` / `wsRef` here: that would
      // belong to the OTHER conversation the user did not intend to
      // stop.
      console.log(
        `[WS-CLOSE] site=cancel skipped: requested=${sessionId ?? "(none)"} active=${currentStreamId ?? "(none)"}`,
      );
      return false;
    }
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
    // even if settle() somehow didn't run.
    // Log the real caller instead of claiming user intent: `cancel()` has
    // TWO callers — the Composer stop button AND `chatStateReset`
    // (session/model switch). The old hardcoded "用户点击停止" text fired
    // for both, which made "the stream died by itself" reports
    // undiagnosable.
    const cancelCaller =
      new Error()
        .stack?.split("\n")
        .slice(2, 4)
        .map((l) => l.trim())
        .join(" <- ") ?? "unknown";
    console.log(`[WS-CLOSE] site=cancel caller=${cancelCaller}`);
    wsRef.current?.();
    wsRef.current = null;
    streamingSessionIdRef.current = null;
    setStreamingSessionId(null);
    return true;
  }, []);

  // 稳定引用的 API 对象：每次 render 都新建字面量会让 StreamChatContext 的
  // value 变化 → 所有消费者无条件重渲染。setters 由 useState 保证稳定，
  // send/cancel/wsSendRaw 是 useCallback 常量。
  const api = useMemo(
    () => ({
      send,
      streaming: streamingSessionId !== null,
      streamingSessionId,
      streamingStatus,
      setStreamingStatus,
      lastActivityAt,
      compactionStatus,
      setCompactionStatus,
      contextUsage,
      setContextUsage,
      usageSnapshot,
      setUsageSnapshot,
      cancel,
      wsSendRaw,
    }),
    [send, cancel, wsSendRaw, streamingSessionId, streamingStatus, lastActivityAt, compactionStatus, contextUsage, usageSnapshot],
  );
  return api;
}

export type StreamChatApi = ReturnType<typeof useStreamChat>;
