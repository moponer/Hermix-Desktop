import { IPC } from "../ipc/commands";
import type { MutableRefObject, Dispatch, SetStateAction } from "react";import { invoke } from "../ipc";
import { reportError } from "../lib/reportError";
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
import { docCanvasStore } from "@hermix/preview";

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
 * Per-send mutable locals (fullText / phase / hasContent / ...) are plain
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
        // [activity: ...] / [status: ...] as Text events (before the lib.rs fix
        // was compiled), silently drop them so they never pollute message body.
        if (piece && !/^\[(?:activity|status):\s*/.test(piece)) {
          ctx.hasContent = true;
          ctx.hasAnyStreamEvent = true;
          ctx.setLastActivityAt(Date.now());
          // 文档画布分流（2026-10-13）：`<hermix-canvas>` marker 之后的
          // 增量归画布（store 吸收，块级缓冲后经 postMessage 上屏），
          // marker 之前的归聊天气泡。feed 返回本次应进气泡的文本（可能
          // 为空）；跨 chunk 边界的 marker 由 store 缓冲拼合。
          // 见 lib/docCanvasStore.ts 顶部协议与不变量说明。
          const bubblePiece = docCanvasStore.feed(piece);
          if (bubblePiece) {
            ctx.fullText += bubblePiece;
            ctx.chunkBuffer += bubblePiece;
            ctx.scheduleChunkFlush();
          }
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
        // 参数原样转发：tool_start 帧里本来就有（后端
        // local_agent_service 的 `"arguments": args`），此前只转 content，
        // 于是「刚才发生」只能写工具名 —— 屏幕上三行 `terminal`，用户看不出
        // 跑的是什么命令（2026-11-06 实报）。
        const toolArgs = data?.arguments ?? data?.event?.arguments;
        ctx.callbacks.onToolStart?.({
          toolCallId,
          tool,
          content: evtContent,
          args: toolArgs,
        });
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

      if (evtType === "member_event") {
        // 成员工单实时事件（总管方案 P4.4）：与 sub_agent_event 平行，
        // 驱动「成员工单」面板（MemberPanel）。
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data;
        ctx.callbacks.onMemberEvent?.({
          memberId: ev?.member_id,
          phase: ev?.phase,
          task: typeof ev?.task === "string" ? ev.task : "",
          turns: typeof ev?.turns === "number" ? ev.turns : undefined,
          error: typeof ev?.error === "string" ? ev.error : undefined,
          recentSteps: Array.isArray(ev?.recent_steps)
            ? ev.recent_steps.filter((s: unknown) => typeof s === "string")
            : undefined,
        });
        return;
      }

      if (evtType === "resource_alert") {
        // 资源闸门边沿（总管方案 P0.5）：breach/recovered 弹通知。
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data;
        ctx.callbacks.onResourceAlert?.(
          String(ev?.kind ?? ""),
          String(ev?.message ?? ""),
        );
        return;
      }

      // Orchestration-bus events (bridged in `ws.rs`). Without these
      // branches the server's Task lifecycle / automation / plan
      // events arrived on the wire and were silently dropped by the
      // switch below. The caller wires the subset the desktop uses;
      // unhandled variants stay no-ops here.
      if (
        evtType === "task_created" ||
        evtType === "task_updated" ||
        evtType === "task_status_changed" ||
        evtType === "automation_triggered" ||
        evtType === "result_delivered" ||
        evtType === "loop_phase_changed" ||
        evtType === "plan_confirmation_required"
      ) {
        ctx.hasAnyStreamEvent = true;
        ctx.setLastActivityAt(Date.now());
        const ev = data?.event ?? data;
        if (evtType === "task_created") {
          ctx.callbacks.onOrchestrationEvent?.({
            type: "task_created",
            taskId: String(ev?.task_id ?? ""),
            title: String(ev?.title ?? ""),
            mode: String(ev?.mode ?? ""),
            workspacePath: String(ev?.workspace_path ?? ""),
          });
        } else if (evtType === "task_updated") {
          ctx.callbacks.onOrchestrationEvent?.({
            type: "task_updated",
            taskId: String(ev?.task_id ?? ""),
            title: String(ev?.title ?? ""),
            status: String(ev?.status ?? ""),
          });
        } else if (evtType === "task_status_changed") {
          ctx.callbacks.onOrchestrationEvent?.({
            type: "task_status_changed",
            taskId: String(ev?.task_id ?? ""),
            oldStatus: String(ev?.old_status ?? ""),
            newStatus: String(ev?.new_status ?? ""),
          });
        } else if (evtType === "automation_triggered") {
          ctx.callbacks.onOrchestrationEvent?.({
            type: "automation_triggered",
            automationId: String(ev?.automation_id ?? ""),
            automationName: String(ev?.automation_name ?? ""),
            taskId: String(ev?.task_id ?? ""),
          });
        } else if (evtType === "result_delivered") {
          ctx.callbacks.onOrchestrationEvent?.({
            type: "result_delivered",
            taskId: String(ev?.task_id ?? ""),
          });
        } else if (evtType === "loop_phase_changed") {
          const stage = String(ev?.stage ?? "");
          const stageLabel: Record<string, string> = {
            perception: "感知",
            reasoning: "推理",
            action: "执行",
            observation: "观察",
          };
          const label = stageLabel[stage] ?? stage;
          ctx.setStreamingStatus(label ? `阶段：${label}` : null);
          ctx.callbacks.onOrchestrationEvent?.({
            type: "loop_phase_changed",
            sessionId: typeof ev?.session_id === "string" ? ev.session_id : null,
            taskId: typeof ev?.task_id === "string" ? ev.task_id : null,
            phase: String(ev?.phase ?? ""),
            stage,
            turn: typeof ev?.turn === "number" ? ev.turn : 0,
          });
        } else {
          ctx.callbacks.onOrchestrationEvent?.({
            type: "plan_confirmation_required",
            taskId: String(ev?.task_id ?? ""),
            planText: String(ev?.plan_text ?? ""),
          });
        }
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
        // 权威增删行数（terminal 差分路径给真 diff）；缺失时前端回落估算。
        const added = typeof ev?.added === "number" ? ev.added : undefined;
        const removed = typeof ev?.removed === "number" ? ev.removed : undefined;
        ctx.callbacks.onFileChange?.({
          toolCallId,
          filePath,
          changeType,
          fileSummary,
          ...(added != null ? { added } : {}),
          ...(removed != null ? { removed } : {}),
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
      // model..."). Displayed as a chip in StreamingBubble, NOT injected into
      // message text.
      if (evtType === "status" || evtType === "activity") {
        const content =
          typeof evtContent === "string" ? evtContent : "";
        // 后端把 section_gate / prefetch_intent / acui 三类结构化事件
        // 包成 `Status{content: <raw extra JSON>}`（ws.rs 的
        // "section_gate" | "prefetch_intent" | "acui" 分支）。该包装
        // 让下方三个专用分支永远不可达（status 先 return），chip 还会
        // 闪出原始 JSON。在此解包并直接执行对应动作，恢复事件链。
        const unwrapped = (() => {
          if (!content) return null;
          try {
            const parsed = JSON.parse(content) as { event?: unknown };
            const kind = typeof parsed?.event === "string" ? parsed.event : null;
            if (kind === "section_gate" || kind === "prefetch_intent" || kind === "acui") {
              return { kind, parsed };
            }
          } catch {
            /* not a wrapped structured event */
          }
          return null;
        })();
        if (unwrapped) {
          if (unwrapped.kind === "section_gate") {
            const narrowed = narrowSectionGateReport(
              (unwrapped.parsed as { report?: unknown }).report ?? null,
            );
            if (narrowed) ctx.callbacks.onSectionGate?.(narrowed);
            return;
          }
          if (unwrapped.kind === "prefetch_intent") {
            const ev = unwrapped.parsed as {
              intent?: unknown; confidence?: unknown; results?: unknown;
            };
            const intent = typeof ev.intent === "string" ? ev.intent : "unknown";
            const confidence = typeof ev.confidence === "number" ? ev.confidence : 0;
            const results: PrefetchResultEntry[] = Array.isArray(ev.results)
              ? (ev.results as Array<Record<string, unknown>>).map((r) => ({
                  tool: typeof r?.tool === "string" ? r.tool : "unknown",
                  args: r?.args ?? {},
                  output: typeof r?.output === "string" ? r.output : "",
                  errored: !!r?.errored,
                  durationMs: typeof r?.duration_ms === "number" ? r.duration_ms : 0,
                }))
              : [];
            ctx.onPrefetchRef.current?.({ intent, confidence, results });
            return;
          }
          const directive = (unwrapped.parsed as { directive?: unknown }).directive;
          if (directive && typeof directive === "object" && "op" in directive) {
            ctx.callbacks.onAcuiDirective?.(directive as AcuiDirective);
          }
          return;
        }
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
              // `expired` = 前端墙钟到期。它**不**代表服务端已结算：后端
              // 计时器按「可送达时间」计费（approval.rs 超时任务），无人
              // 在线时暂停、最长挂到 30 分钟硬上限。此处若吞掉决定，
              // agent 循环会一直挂在审批 oneshot 上，整条输出流冻死在
              // 卡片位置（2026-11-09 实测）。所以 expired 照常送达——
              // 映射为 denied（fail-closed，与后端超时的默认行为一致）；
              // 服务端已结算时 resolve 只是 matched=false 的幂等无操作。
              // 流占位符恢复仅限非 expired：过期卡的轮次多半已推进，
              // 恢复占位符只会留下幽灵转圈。
              const expiredSettled = resp.decision === "expired";
              if (!expiredSettled) {
                // Restore the streaming placeholder cleared when the card
                // opened. There is a ~1s gap between the user's click and the
                // agent loop's next WS event; without this the chat shows dead
                // whitespace where the waiting card used to be.
                ctx.streamingSessionIdRef.current = ctx.sessionId;
                ctx.setStreamingSessionId(ctx.sessionId);
              }
              // No `wsRef.current` gate: `backend_channel_send` targets the
              // backend channel registry, not the frontend subscription, so
              // a null `unlisten` says nothing about whether the approval
              // registry is still listening. Gating on it silently dropped
              // user decisions. Send unconditionally; a real failure now
              // surfaces instead of vanishing.
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
                // 拒绝时可附「接下来该怎么做」——服务端把 note 折进 LLM
                // 可见的拒绝理由（DeniedWithFeedback），模型据此换方案。
                const note = resp.note?.trim() || undefined;
                const payload = JSON.stringify({
                  type: "approval_response",
                  request_id: requestId,
                  decision,
                  ...(note ? { note } : {}),
                });
                void invoke<void>(IPC.backendChannelSend, {
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
              // No `wsRef.current` gate：backend_channel_send 打的是后端通道注册表，
              // 与前端订阅无关；闸门会静默丢用户决定。
              try {
                void invoke<void>(IPC.backendChannelSend, {
                  channel: streamChannelName(ctx.sessionId),
                  payload: JSON.stringify({
                    type: "approval_response",
                    request_id: requestId,
                    decision: "denied",
                  }),
                });
              } catch (e) { reportError("stream-parser", e); }
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
          // non-file tools (run_command / http / ...) leave it absent.
          diff: typeof it?.diff === "string" ? (it.diff as string) : null,
        }));
        if (ctx.callbacks.onApprovalBatchRequest) {
          ctx.callbacks
            .onApprovalBatchRequest({ batchId, items, sessionId: ctx.sessionId })
            .then((resp) => {
              ctx.approvalPending = false;
              ctx.armTimeouts();
              // 与单条路径同因（2026-11-09）：expired 不代表服务端已结算
              // （后端计时器按可送达时间计费、无人在线时暂停），吞掉决定
              // 会把 agent 循环冻死在 oneshot 上。照常送达（映射为 denied，
              // 服务端已结算则 matched=false 幂等无操作）；expired 跳过
              // 流占位符恢复，避免幽灵转圈。
              const expiredSettled = resp.decision === "expired";
              if (!expiredSettled) {
                ctx.streamingSessionIdRef.current = ctx.sessionId;
                ctx.setStreamingSessionId(ctx.sessionId);
              }
              // No `wsRef.current` gate（与单条路径同因）：backend_channel_send
              // 打的是后端通道注册表，与前端订阅无关；闸门会静默丢用户决定。
              try {
                const decision =
                  resp.remember === true
                    ? "always"
                    : resp.decision === "approved"
                      ? "approved"
                      : "denied";
                const note = resp.note?.trim() || undefined;
                const payload = JSON.stringify({
                  type: "approval_responses_batch",
                  batch_id: batchId,
                  decision,
                  ...(note ? { note } : {}),
                });
                void invoke<void>(IPC.backendChannelSend, {
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
              // No `wsRef.current` gate：backend_channel_send 打的是后端通道注册表，
              // 与前端订阅无关；闸门会静默丢用户决定。
              try {
                void invoke<void>(IPC.backendChannelSend, {
                  channel: streamChannelName(ctx.sessionId),
                  payload: JSON.stringify({
                    type: "approval_responses_batch",
                    batch_id: batchId,
                    decision: "denied",
                  }),
                });
              } catch (e) { reportError("stream-parser", e); }
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
              // No `wsRef.current` guard here (unlike the approval path):
              // `backend_channel_send` writes into the *backend* channel
              // registry, which is independent of whether the frontend
              // still holds a live event subscription. Gating on
              // `wsRef.current` therefore discarded user answers whenever
              // the stream had already settled — leaving the agent blocked
              // on its own clarify timeout with no visible error. Send
              // unconditionally and report a genuine send failure.
              try {
                void invoke<void>(IPC.backendChannelSend, {
                  channel: streamChannelName(ctx.sessionId),
                  payload: JSON.stringify({
                    type: "clarify_response",
                    request_id: requestId,
                    answer: resp.answer ?? "",
                  }),
                }).catch((e) => {
                  console.error(
                    `[clarify] backend_channel_send failed for ${requestId}: ${String(e)}`,
                  );
                });
              } catch (e) {
                console.error(
                  `[clarify] backend_channel_send threw for ${requestId}: ${String(e)}`,
                );
              }
            })
            .catch((e) => {
              // The user dismissed/cancelled the clarify card. Still answer
              // with an empty string so the agent's `rx` resolves and the
              // turn unblocks instead of waiting out the clarify timeout.
              console.warn(
                `[clarify] no answer for ${requestId} (${String(e)}); sending empty response to unblock the agent`,
              );
              try {
                void invoke<void>(IPC.backendChannelSend, {
                  channel: streamChannelName(ctx.sessionId),
                  payload: JSON.stringify({
                    type: "clarify_response",
                    request_id: requestId,
                    answer: "",
                  }),
                }).catch(() => {
                  /* logged above */
                });
              } catch {
                /* best effort */
              }
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
        // 文档画布：流结束强制收口。开启态下未闭合块的缓冲整体上屏；
        // 未开启态的疑似 marker 前缀残留（流断在字面量上）补回气泡，
        // store 不静默吞正文。
        const canvasLeftover = docCanvasStore.close();
        if (canvasLeftover) {
          ctx.fullText += canvasLeftover;
          ctx.chunkBuffer += canvasLeftover;
          ctx.scheduleChunkFlush();
        }
        const reason: string | undefined =
          data?.finish_reason || data?.event?.finish_reason;
        // Terminal outcome classification from the server.
        const outcome: string | undefined =
          data?.outcome || data?.event?.outcome;
        const rawErrorBase: string | undefined =
          data?.error || data?.event?.error;
        // C3-2：用户主动停止（interrupted 且无错误痕迹）是**正常收尾**，
        // 不是后端错误——之前被误分类进错误分支，停止后报"对话因后端错误
        // 中断"。后端错误场景（Error event 已先行）的 Done 带 rawError 且
        // finish_reason=error，据此与用户中断区分。
        const isUserInterrupt =
          outcome === "interrupted" && reason !== "error" && !rawErrorBase;
        const isFailedOutcome =
          outcome === "failed" || (reason === "error" && !isUserInterrupt);
        const rawError: string | undefined = isUserInterrupt
          ? undefined
          : rawErrorBase;
        if (isFailedOutcome || (reason === "error" && !isUserInterrupt) || (!!rawError && !isUserInterrupt)) {
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
        // 文档画布：错误路径同样收口，残留补回气泡。
        const canvasLeftover = docCanvasStore.close();
        if (canvasLeftover) {
          ctx.fullText += canvasLeftover;
          ctx.chunkBuffer += canvasLeftover;
          ctx.scheduleChunkFlush();
        }
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
