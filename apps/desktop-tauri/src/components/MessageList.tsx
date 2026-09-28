import { memo, useState, useRef, useEffect, useLayoutEffect, useCallback, forwardRef, useImperativeHandle } from "react";
import { ArrowDown } from "lucide-react";
import {
  shouldFollowOnStreamChunk,
  computeBottomScrollTop,
  distanceFromBottomPx,
  isUserScrolledUp,
  scrollToBottomWithSettle,
  AT_BOTTOM_TOLERANCE_PX,
  USER_INTENT_WINDOW_MS,
} from "../utils/chatScroll";
import { MessageBubble } from "./MessageBubble";
import { AgentActivityIndicator, WaitForInputIndicator } from "./ChatIndicators";
import { StreamingBubble } from "./StreamingBubble";
import { TodoListCard } from "./TodoListCard";
import { SubAgentPanel } from "./SubAgentPanel";
import { CompactionStatusBanner } from "./CompactionStatusBanner";
import { PrefetchIntentChip } from "./MessageBubble";
import { shouldShowAssistantHeader } from "./chatUtils";
import type { ChatMessage, SubAgentState } from "../types";
import type { StreamPhase, TodoSnapshotItem, StreamChatApi } from "../hooks/useStreamChat";

export type TodoGroup = {
  groupId: string;
  groupTitle: string | null;
  anchorMessageId: string | null;
  itemsById: Record<string, TodoSnapshotItem>;
};

export type StreamRow =
  | { kind: "message"; msg: ChatMessage; role: ChatMessage["role"] }
  | { kind: "todo"; group: TodoGroup; role: undefined };

export interface MessageListHandle {
  scrollToBottom: (force: boolean) => void;
}

export interface MessageListProps {
  session: { id?: string; messages: ChatMessage[]; title?: string; message_count?: number } | null;
  /** 历史翻页：hasMore 时列表顶部显示「加载更早的消息」，onLoadMore 前插一页（App 负责取数）。 */
  historyPaging?: { hasMore: boolean; loading: boolean; onLoadMore: () => void } | null;
  stream: StreamChatApi;
  isStreaming: boolean;
  isAgentActiveState: boolean;
  forceScrollSeq: number;
  setForceScrollSeq: React.Dispatch<React.SetStateAction<number>>;
  postStreamActivity: boolean;
  pendingToolsCount: number;
  streamingAssistantId: string | null;
  streamingMsg: ChatMessage | null;
  streamingPhase?: StreamPhase | null;
  hasPendingApproval: boolean;
  activeTurnMsgIds: Set<string>;
  streamRow: StreamRow[];
  showHeaderFlags: boolean[];
  subAgents?: Record<string, SubAgentState>;
  prefetchIntent?: {
    intent: string;
    confidence: number;
    results: Array<{ tool: string; args: unknown; output: string; errored: boolean; durationMs: number }>;
  } | null;
  inlineCompactionDismissed: boolean;
  setInlineCompactionDismissed: (v: boolean) => void;
  expertCutoffAt: number | null;
  onTodoPatch?: (id: string, patch: { status?: string; title?: string; priority?: string }) => Promise<unknown>;
  justPatchedTodoRef?: React.MutableRefObject<Set<string>>;
  onPathClick?: (path: string) => void;
  onApproveApproval?: (requestId: string, remember?: boolean) => void;
  onDenyApproval?: (requestId: string) => void;
  onApproveBatchApproval?: (batchId: string, remember?: boolean) => void;
  onDenyBatchApproval?: (batchId: string) => void;
  onReasoningFoldChange?: (folded: boolean) => void;
  onAnswerClarify?: (requestId: string, answer: string) => void;
  workspaceRoot?: string | null;
  sending: boolean;
  /** Captured reasoning lines for the bottom-of-chat ticker. */
  reasoningLines?: string[];
  /** Whether the latest reported fold state is "folded". */
  reasoningFolded?: boolean;
}

// 距底部多少像素内仍算"贴底"、以及真实滚动输入的意图窗口，
// 见 utils/chatScroll.ts —— 判定逻辑集中在那里以便单测。
//
// 这是"流式输出时自动滚动突然停住"的根因：旧逻辑在 scroll 事件里直接用
// `dist > 0` 判定用户上滚，而程序写完 scrollTop 之后内容又长高，同一个
// scroll 事件里读到的 dist 已经 > 0 —— 被误判成用户上滚。误判一旦发生就
// 不可逆：hasUserScrolledUp 变 true 后自动滚动被禁用，容器不再滚动，也就
// 不再有 scroll 事件把它复位成 false，于是永久停摆。
const MemoMessageBubble = memo(MessageBubble, (prev, next) => {
  if (prev.message !== next.message) return false;
  if (prev.workspaceRoot !== next.workspaceRoot) return false;
  if (prev.onApproveApproval !== next.onApproveApproval) return false;
  if (prev.onDenyApproval !== next.onDenyApproval) return false;
  if (prev.onApproveBatchApproval !== next.onApproveBatchApproval) return false;
  if (prev.onDenyBatchApproval !== next.onDenyBatchApproval) return false;
  if (prev.onReasoningFoldChange !== next.onReasoningFoldChange) return false;
  if (prev.onAnswerClarify !== next.onAnswerClarify) return false;
  if (prev.showHeader !== next.showHeader) return false;
  if (prev.isStreaming !== next.isStreaming) return false;
  if (prev.onPathClick !== next.onPathClick) return false;
  if (prev.expertCutoffAt !== next.expertCutoffAt) return false;
  if (prev.keepOpen !== next.keepOpen) return false;
  return true;
});

export const MessageList = forwardRef<MessageListHandle, MessageListProps>(function MessageList(props, ref) {
  const {
    session,
    historyPaging,
    stream,
    isStreaming,
    isAgentActiveState,
    forceScrollSeq,
    setForceScrollSeq,
    postStreamActivity,
    pendingToolsCount,
    streamingAssistantId,
    streamingMsg,
    streamingPhase,
    hasPendingApproval,
    activeTurnMsgIds,
    streamRow,
    showHeaderFlags,
    subAgents,
    prefetchIntent,
    inlineCompactionDismissed,
    setInlineCompactionDismissed,
    expertCutoffAt,
    onTodoPatch,
    justPatchedTodoRef,
    onPathClick,
    onApproveApproval,
    onDenyApproval,
    onApproveBatchApproval,
    onDenyBatchApproval,
    onReasoningFoldChange,
    onAnswerClarify,
    workspaceRoot,
    sending,
    reasoningLines,
    reasoningFolded,
  } = props;

  const scrollContainerRef = useRef<HTMLDivElement>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const showScrollToBottomRef = useRef(false);
  const [showScrollToBottom, setShowScrollToBottom] = useState(false);
  const scrollPendingRef = useRef(false);
  const lastObservedForceSeqRef = useRef(0);
  const lastScrollHeightRef = useRef(0);
  const prevSessionIdRef = useRef<string | undefined>(undefined);
  const justRestoredRef = useRef(false);
  const hasUserScrolledUpRef = useRef(false);
  // 最近一次真实滚动输入的时刻（wheel / 触摸 / 键盘 / 拖动滚动条）。
  const userScrollIntentAtRef = useRef(0);
  // 历史前插锚定：记录上一轮 commit 的（loading, scrollHeight），
  // 前插完成的那次 commit 用高度差把视口锚回原先看到的内容。
  const historyAnchorRef = useRef<{ loading: boolean; height: number } | null>(null);
  // 前插后屏蔽一次「自动贴底」effect——否则 messages.length 变化会把
  // 正在读历史的用户拽回底部。
  const skipAutoBottomOnceRef = useRef(false);

  const syncShowScrollToBottom = (visible: boolean) => {
    if (showScrollToBottomRef.current === visible) return;
    showScrollToBottomRef.current = visible;
    setShowScrollToBottom(visible);
  };

  const scrollToBottom = useCallback((force: boolean) => {
    const el = scrollContainerRef.current;
    if (!el) return;
    if (!shouldFollowOnStreamChunk(hasUserScrolledUpRef.current, force)) {
      return;
    }
    if (force) {
      // 显式强制滚动（发送 / 点回到底部）意味着用户想回到最新，
      // 必须清掉"已上滚"状态，否则后续流式块仍被粘性守卫拦住。
      hasUserScrolledUpRef.current = false;
      userScrollIntentAtRef.current = 0;
      syncShowScrollToBottom(false);
    }
    el.scrollTop = computeBottomScrollTop(el.scrollHeight, el.clientHeight);
  }, []);

  useImperativeHandle(ref, () => ({ scrollToBottom }), [scrollToBottom]);

  useEffect(() => {
    scrollPendingRef.current = true;
  }, [session?.messages, pendingToolsCount, postStreamActivity]);

  // 流式跟随：用 ResizeObserver 监听内容高度变化，替代旧的 rAF 轮询。
  // 旧实现每帧读一次 scrollHeight（强制同步布局）+ 写 scrollTop，agent
  // 活跃期间把主线程打满，任何交互都要排队。RO 只在高度真正变化时回调。
  const agentActiveRef = useRef(false);
  useEffect(() => {
    agentActiveRef.current = isAgentActiveState;
    if (!isAgentActiveState) {
      lastScrollHeightRef.current = scrollContainerRef.current?.scrollHeight ?? 0;
    }
  }, [isAgentActiveState]);

  const contentRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const content = contentRef.current;
    if (!content) return;
    const ro = new ResizeObserver((entries) => {
      const measured = entries[0]?.contentRect.height ?? 0;
      if (!agentActiveRef.current) {
        // 非流式也要消费 pending：消息数组变化（含冷挂载首批落地）会置位，
        // 只在 agent 活跃时消费的话，历史分批加载完就停在顶部 —— 浮窗（
        // 工作台对话）每次展开都是全新挂载，正好踩中这条路径。
        // 用户已上滚时 scrollToBottom(false) 被粘性守卫拦下，不会拽人。
        if (scrollPendingRef.current && measured > 0) {
          scrollPendingRef.current = false;
          scrollToBottom(false);
        }
        lastScrollHeightRef.current = measured || lastScrollHeightRef.current;
        return;
      }
      const height = measured;
      const heightGrew = height > lastScrollHeightRef.current;
      lastScrollHeightRef.current = height;
      if (scrollPendingRef.current || heightGrew) {
        scrollPendingRef.current = false;
        scrollToBottom(false);
      }
    });
    ro.observe(content);
    return () => ro.disconnect();
  }, [scrollToBottom]);

  // 显式强制滚动（发送 / 点回到底部）不等内容高度变化，seq 变化即执行。
  // 非流式路径由下方 commit effect 兜底，两处共用 lastObservedForceSeqRef 去重。
  useEffect(() => {
    if (!isStreaming) return;
    if (forceScrollSeq === lastObservedForceSeqRef.current) return;
    lastObservedForceSeqRef.current = forceScrollSeq;
    scrollToBottom(true);
  }, [isStreaming, forceScrollSeq, scrollToBottom]);

  useEffect(() => {
    const currentId = session?.id;
    if (prevSessionIdRef.current === currentId) return;
    prevSessionIdRef.current = currentId;
    hasUserScrolledUpRef.current = false;
    showScrollToBottomRef.current = false;
    setShowScrollToBottom(false);
    justRestoredRef.current = true;
  }, [session?.id]);

  // 真实输入事件打意图标记。程序化写 scrollTop 不经过这里，
  // 因此流式跟随不会被自己触发的 scroll 事件误判成用户上滚。
  useEffect(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    const mark = () => {
      userScrollIntentAtRef.current = Date.now();
    };
    el.addEventListener("wheel", mark, { passive: true });
    el.addEventListener("touchstart", mark, { passive: true });
    el.addEventListener("touchmove", mark, { passive: true });
    el.addEventListener("pointerdown", mark, { passive: true });
    el.addEventListener("keydown", mark);
    return () => {
      el.removeEventListener("wheel", mark);
      el.removeEventListener("touchstart", mark);
      el.removeEventListener("touchmove", mark);
      el.removeEventListener("pointerdown", mark);
      el.removeEventListener("keydown", mark);
    };
  }, []);

  const handleContainerScroll = useCallback(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    const dist = distanceFromBottomPx(el.scrollTop, el.scrollHeight, el.clientHeight);
    const atBottom = dist <= AT_BOTTOM_TOLERANCE_PX;
    const hasIntent =
      Date.now() - userScrollIntentAtRef.current < USER_INTENT_WINDOW_MS;
    const isUp = isUserScrolledUp(dist, hasIntent);
    if (isUp !== hasUserScrolledUpRef.current) {
      hasUserScrolledUpRef.current = isUp;
      // 用户自己滚回底部 → 立即恢复跟随，不必等下一次内容增长。
      if (!isUp) {
        scrollPendingRef.current = true;
        // 旧 rAF 轮询会在下一帧消费这个标记；改为 RO 后安排一次性消费。
        requestAnimationFrame(() => {
          if (scrollPendingRef.current) {
            scrollPendingRef.current = false;
            scrollToBottom(false);
          }
        });
      }
    }
    syncShowScrollToBottom(!atBottom);
  }, [scrollToBottom]);

  // 历史前插锚定（每次 commit 后跑，开销是一次 scrollHeight 读数）：
  // loading true→false 的那次 commit 就是前插落地，DOM 已含新高度，
  // 在绘制前按高度差平移 scrollTop，用户正在看的那条消息原地不动。
  useLayoutEffect(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    const prev = historyAnchorRef.current;
    const loading = !!historyPaging?.loading;
    if (prev?.loading && !loading) {
      const delta = el.scrollHeight - prev.height;
      if (delta > 0) {
        el.scrollTop += delta;
        skipAutoBottomOnceRef.current = true;
      }
    }
    historyAnchorRef.current = { loading, height: el.scrollHeight };
  });

  useEffect(() => {
    if (isStreaming) return;
    if (skipAutoBottomOnceRef.current) {
      skipAutoBottomOnceRef.current = false;
      return;
    }
    const force = justRestoredRef.current || forceScrollSeq > 0;
    if (!force && hasUserScrolledUpRef.current) return;
    const el = scrollContainerRef.current;
    if (!el) return;
    const isRestore = justRestoredRef.current;
    scrollToBottomWithSettle(
      el,
      isRestore ? 15 : 0,
      () => hasUserScrolledUpRef.current && !justRestoredRef.current,
    );
    if (isRestore) {
      justRestoredRef.current = false;
    }
    if (forceScrollSeq > 0 && forceScrollSeq === lastObservedForceSeqRef.current) {
      setForceScrollSeq(0);
    } else if (forceScrollSeq > 0) {
      lastObservedForceSeqRef.current = forceScrollSeq;
    }
  }, [session?.messages?.length, isStreaming, forceScrollSeq, setForceScrollSeq]);

  return (
    <div className="relative flex-1 min-h-0">
      <div
        ref={scrollContainerRef}
        onScroll={handleContainerScroll}
        className="absolute inset-0 overflow-y-auto px-3 sm:px-4 py-5"
        // 关掉浏览器原生滚动锚定：上方内容（工具卡片展开、图片加载）变化时
        // 它会自行调整 scrollTop，把已经贴底的视口顶离底部。
        style={{ overflowAnchor: "none" }}
      >
        <div ref={contentRef} className="msg-stream-list max-w-reading mx-auto space-y-5">
          {historyPaging?.hasMore && (
            <div className="flex justify-center pt-1">
              <button
                type="button"
                onClick={historyPaging.onLoadMore}
                disabled={historyPaging.loading}
                className="text-xs px-3 py-1 rounded-md border border-[color:var(--color-border-secondary)] text-[color:var(--color-text-secondary)] hover:text-[color:var(--color-text-primary)] hover:border-[color:var(--color-border-primary)] disabled:opacity-50 transition-colors"
              >
                {historyPaging.loading ? "正在加载更早的消息…" : "加载更早的消息"}
              </button>
            </div>
          )}
          {streamRow.map((row, idx) => {
            if (row.kind === "todo") {
              return (
                <div key={`todo-${row.group.groupId}`} className="todo-anchor">
                  <TodoListCard
                    groupId={row.group.groupId}
                    groupTitle={row.group.groupTitle}
                    itemsById={row.group.itemsById}
                    onPatch={onTodoPatch}
                    justPatchedRef={justPatchedTodoRef}
                  />
                </div>
              );
            }
            const msg = row.msg;
            if (streamingAssistantId && msg.id === streamingAssistantId && streamingMsg) {
              const rawIdx = session?.messages.findIndex((m) => m.id === streamingAssistantId) ?? -1;
              const showStreamHeader = rawIdx >= 0
                ? shouldShowAssistantHeader(rawIdx, session?.messages ?? [])
                : true;
              return (
                <StreamingBubble
                  key={msg.id}
                  text={streamingMsg.content}
                  phase={streamingPhase ?? null}
                  statusText={stream.streamingStatus}
                  showHeader={showStreamHeader}
                  reasoningContent={streamingMsg.reasoningContent}
                  executionTimeline={streamingMsg.execution_timeline}
                  unifiedSteps={streamingMsg.unified_steps}
                  keepOpen={hasPendingApproval || pendingToolsCount > 0}
                />
              );
            }
            const showHeader = msg.role !== "assistant" || showHeaderFlags[idx];
            return (
              <MemoMessageBubble
                key={msg.id}
                message={msg}
                workspaceRoot={workspaceRoot}
                onApproveApproval={onApproveApproval}
                onDenyApproval={onDenyApproval}
                onApproveBatchApproval={onApproveBatchApproval}
                onDenyBatchApproval={onDenyBatchApproval}
                onReasoningFoldChange={onReasoningFoldChange}
                onAnswerClarify={onAnswerClarify}
                showHeader={showHeader}
                isStreaming={!!streamingAssistantId && msg.id === streamingAssistantId}
                keepOpen={
                  msg.id !== streamingAssistantId && activeTurnMsgIds.has(msg.id)
                }
                hasRunningTool={
                  msg.id !== streamingAssistantId && activeTurnMsgIds.has(msg.id)
                }
                onPathClick={onPathClick}
                expertCutoffAt={expertCutoffAt}
              />
            );
          })}

          {(() => {
            const phase = stream.compactionStatus?.phase;
            const active =
              phase === "starting" ||
              phase === "in_progress_fast" ||
              phase === "in_progress_llm";
            if (!active || inlineCompactionDismissed) return null;
            return (
              <div className="compaction-inline-card pl-[32px]">
                <CompactionStatusBanner
                  status={stream.compactionStatus}
                  onDismiss={() => setInlineCompactionDismissed(true)}
                />
              </div>
            );
          })()}

          <SubAgentPanel subAgents={subAgents ?? {}} />

          {streamingMsg && prefetchIntent && (
            <PrefetchIntentChip
              intent={prefetchIntent.intent}
              confidence={prefetchIntent.confidence}
            />
          )}

          {isStreaming && !streamingMsg && session && (
            <StreamingBubble
              key="streaming-fallback"
              text=""
              phase={streamingPhase ?? null}
              statusText={stream.streamingStatus}
              showHeader
            />
          )}

          {(() => {
            const recentActivityWindowMs = 4000;
            const hasRecentActivity =
              typeof stream.lastActivityAt === "number" &&
              Date.now() - stream.lastActivityAt < recentActivityWindowMs;
            const busy =
              isStreaming ||
              sending ||
              !!streamingAssistantId ||
              postStreamActivity ||
              pendingToolsCount > 0 ||
              hasRecentActivity;
            if (busy) {
              return (
                <AgentActivityIndicator
                  pendingToolsCount={pendingToolsCount}
                  visible
                  replying={isStreaming}
                  reasoningLines={reasoningLines}
                  reasoningFolded={reasoningFolded ?? false}
                />
              );
            }
            const lastAssistant = session?.messages
              ?.slice()
              .reverse()
              .find((m) => m.role === "assistant");
            const noVisibleOutput = !!lastAssistant &&
              (lastAssistant.content ?? "").trim().length === 0 &&
              (
                (lastAssistant.reasoningContent ?? "").trim().length > 0 ||
                (lastAssistant.tool_calls?.length ?? 0) > 0 ||
                (lastAssistant.execution_timeline?.length ?? 0) > 0
              );
            return (
              <WaitForInputIndicator
                visible={!!session && session.messages.length > 0}
                hint={noVisibleOutput ? "已结束，未生成可见回复，可继续追问…" : undefined}
              />
            );
          })()}

          <div ref={messagesEndRef} />
        </div>
      </div>

      {showScrollToBottom && (
        <div className="absolute left-0 right-0 bottom-5 z-20 pointer-events-none">
          <div className="max-w-reading mx-auto px-3 sm:px-4 relative">
            <button
              type="button"
              onClick={() => scrollToBottom(true)}
              className="absolute right-6 bottom-0 flex items-center justify-center w-9 h-9 rounded-full shadow-lg transition-transform hover:scale-105 active:scale-95 pointer-events-auto"
              style={{
                background: "var(--color-bg-elevated)",
                color: "var(--color-text-primary)",
                border: "1px solid var(--color-border-subtle)",
                boxShadow: "0 4px 14px rgba(0,0,0,0.25)",
              }}
              title="滚动到底部"
              aria-label="滚动到底部"
            >
              <ArrowDown className="icon-lg" />
            </button>
          </div>
        </div>
      )}
    </div>
  );
});
