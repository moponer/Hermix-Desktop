import { memo, useState, useRef, useEffect, useCallback, forwardRef, useImperativeHandle } from "react";
import { ArrowDown } from "lucide-react";
import {
  shouldFollowOnStreamChunk,
  computeBottomScrollTop,
  scrollToBottomWithSettle,
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
  onApproveApproval?: (requestId: string) => void;
  onDenyApproval?: (requestId: string) => void;
  onAnswerClarify?: (requestId: string, answer: string) => void;
  workspaceRoot?: string | null;
  sending: boolean;
}

// Memoized wrapper around the full MessageBubble.
const MemoMessageBubble = memo(MessageBubble, (prev, next) => {
  if (prev.message !== next.message) return false;
  if (prev.workspaceRoot !== next.workspaceRoot) return false;
  if (prev.onApproveApproval !== next.onApproveApproval) return false;
  if (prev.onDenyApproval !== next.onDenyApproval) return false;
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
    onAnswerClarify,
    workspaceRoot,
    sending,
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

  const scrollToBottom = useCallback((force: boolean) => {
    const el = scrollContainerRef.current;
    if (!el) return;
    if (!shouldFollowOnStreamChunk(hasUserScrolledUpRef.current, force)) {
      return;
    }
    el.scrollTop = computeBottomScrollTop(el.scrollHeight, el.clientHeight);
  }, []);

  useImperativeHandle(ref, () => ({ scrollToBottom }), [scrollToBottom]);

  useEffect(() => {
    scrollPendingRef.current = true;
  }, [session?.messages, pendingToolsCount, postStreamActivity]);

  useEffect(() => {
    if (!isAgentActiveState) {
      lastScrollHeightRef.current = scrollContainerRef.current?.scrollHeight ?? 0;
      return;
    }
    let raf = 0;
    const tick = () => {
      const el = scrollContainerRef.current;
      if (el) {
        const heightGrew = el.scrollHeight > lastScrollHeightRef.current;
        lastScrollHeightRef.current = el.scrollHeight;
        if (forceScrollSeq !== lastObservedForceSeqRef.current) {
          lastObservedForceSeqRef.current = forceScrollSeq;
          scrollToBottom(true);
        } else if (scrollPendingRef.current || heightGrew) {
          scrollPendingRef.current = false;
          scrollToBottom(false);
        }
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => {
      cancelAnimationFrame(raf);
    };
  }, [isAgentActiveState, scrollToBottom, forceScrollSeq]);

  useEffect(() => {
    const currentId = session?.id;
    if (prevSessionIdRef.current === currentId) return;
    prevSessionIdRef.current = currentId;
    hasUserScrolledUpRef.current = false;
    showScrollToBottomRef.current = false;
    setShowScrollToBottom(false);
    justRestoredRef.current = true;
  }, [session?.id]);

  const handleContainerScroll = useCallback(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    const isUp = dist > 0;
    hasUserScrolledUpRef.current = isUp;
    if (showScrollToBottomRef.current !== isUp) {
      showScrollToBottomRef.current = isUp;
      setShowScrollToBottom(isUp);
    }
  }, []);

  useEffect(() => {
    if (isStreaming) return;
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
      <div ref={scrollContainerRef} onScroll={handleContainerScroll} className="absolute inset-0 overflow-y-auto px-3 sm:px-4 py-5">
        <div className="msg-stream-list max-w-[720px] lg:max-w-[800px] xl:max-w-[920px] mx-auto space-y-5">
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
          <div className="max-w-[720px] lg:max-w-[800px] xl:max-w-[920px] mx-auto px-3 sm:px-4 relative">
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
              <ArrowDown size={18} />
            </button>
          </div>
        </div>
      )}
    </div>
  );
});
