// Text-based bubbles: UserMessage, SystemMessage, AssistantMessage and the
// stop-reason chip. Extracted from MessageBubble.tsx; MessageBubble.tsx keeps
// the dispatcher that routes to these.
import React, { useMemo } from "react";
import { AlertCircle } from "lucide-react";
import { openImageViewer } from "./imageViewer";
import { Avatar } from "./Avatar";
import { formatRelativeTime } from "../utils/time";
import {
  extractFileChanges, extractMemoryUpdates, extractMemoryRecalls, resolveSteps,
} from "./messageParser";
import { formatDuration } from "./bubbleUtils";
import { UnifiedTimelineRenderer } from "./ToolCallBubble";
import { MessageCopyAction } from "./MessageCopyAction";
import type { ChatMessage } from "../types";
export function StopReasonChip({ reason, meta }: { reason: string; meta?: string }) {
  // Normal terminations: render nothing. The content + WaitForInputIndicator
  // already communicate completion; a "工具调用中" chip on a settled
  // message directly contradicts the "可以继续提问" hint below it.
  if (reason === "stop" || reason === "tool_calls") return null;

  const map: Record<string, { label: string; cls: string }> = {
    length:      { label: "已截断",         cls: "text-text-warning" },
    interrupted: { label: "已中断",         cls: "text-text-faint italic" },
    error:       { label: "出错了",         cls: meta === "stream-error" || meta === "non-error" ? "text-text-faint italic" : "text-text-error" },
    timeout:     { label: "超时",           cls: "text-text-warning italic" },
    slow:        { label: "响应较慢",        cls: "text-text-warning" },
    partial:     { label: "未完成",         cls: "text-text-faint italic" },
  };
  const info = map[reason] ?? { label: reason, cls: "text-text-faint italic" };
  return (
    <span className={`h-tiny ${info.cls}`}>{info.label}</span>
  );
}
export function UserMessage({ message }: { message: ChatMessage }) {
  return (
    <div className="msg-user-column h-enter mt-5 msg-copyable">
      <div className="msg-bubble-user whitespace-pre-wrap break-words">
        {message.images && message.images.length > 0 && (
          <div style={{ display: "flex", flexWrap: "wrap", gap: 6, marginBottom: message.content ? 6 : 0 }}>
            {message.images.map((img, i) => (
              <img
                key={`img-${img.name || i}`}
                src={img.blobUrl}
                alt={img.name}
                onClick={() =>
                  openImageViewer(
                    message.images!.map((m) => ({ src: m.blobUrl, name: m.name })),
                    i,
                  )
                }
                style={{ maxWidth: 180, maxHeight: 180, borderRadius: 8, border: "1px solid #2e2e34", objectFit: "cover", cursor: "pointer" }}
                loading="lazy"
              />
            ))}
          </div>
        )}
        {message.content}
      </div>
      <MessageCopyAction
        content={message.content}
        className="message-copy-row message-copy-row--user"
      />
    </div>
  );
}
export function AssistantMessage({
  message, workspaceRoot: _workspaceRoot, showHeader = true, onPathClick, isStreaming, keepOpen = false, hasRunningTool: hasRunningToolProp, expertCutoffAt: _expertCutoffAt,
  onApproveApproval,
  onDenyApproval,
}: {
  message: ChatMessage;
  workspaceRoot?: string | null;
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /**
   * Approval decision handlers. The new path forwards them down to
   * `<UnifiedTimelineRenderer>` so an inline `kind: "approval_request"`
   * step inside this assistant's `unified_steps` can wire its
   * 拒绝 / 允许 buttons. Without these the inline card is rendered
   * read-only — clicking does nothing.
   */
  onApproveApproval?: (requestId: string) => void;
  onDenyApproval?: (requestId: string) => void;
  /**
   * Whether to render the avatar + name + time + model + stop chip.
   * The copy action is rendered below the message body, even though
   * `showHeader` still controls the single per-turn identity header.
   */
  showHeader?: boolean;
  /**
   * Keep the reasoning/tool block expanded even when `isStreaming` is
   * false (agent paused on a pending approval card or a still-running
   * tool). See `MessageBubbleProps.keepOpen` for the full rationale.
   */
  keepOpen?: boolean;
  /**
   * External "is anything still running for this message" signal from
   * ChatView (streamingAssistantId still alive + this msg is the
   * last assistant + pendingToolsCount/hasPendingApproval). When set
   * (i.e. ChatView passed a real boolean, not undefined), it OVERRIDES
   * the internal `useMemo` derivation from message.toolCalls. The
   * internal memo only sees toolCalls whose status="running" locally
   * — it cannot see that the agent loop is *about to call* another
   * tool or that the LLM stream has settled but the WS session is
   * still active. ChatView has those cross-message signals, so when
   * it provides one we trust it.
   *
   * `undefined` (legacy / non-ChatView callers) → fall back to the
   * internal useMemo so older tests/previews keep working.
   */
  hasRunningTool?: boolean;
  /**
   * Whether THIS message is the live streaming message. Only the
   * currently-streaming message is allowed to show the "等待回复"
   * ("waiting for reply") placeholder — historical/reloaded messages
   * never display it, even if their tool list happens to be fully
   * done with empty body. Without this guard, every historical
   * assistant turn in a long conversation that called tools would
   * render a "等待回复..." line below it.
   */
  isStreaming?: boolean;
  /**
   * Watershed timestamp (ms) for an expert switch: messages whose
   * timestamp is below it are pre-switch history (Hermix avatar / original
   * time); at/above it use the new expert's avatar / timeline.
   */
  expertCutoffAt?: number | null;
}) {
  const fileChanges = useMemo(
    () => extractFileChanges(message.execution_timeline),
    [message.execution_timeline],
  );
  const memoryUpdates = useMemo(
    () => extractMemoryUpdates(message.execution_timeline),
    [message.execution_timeline],
  );
  const memoryRecalls = useMemo(
    () => extractMemoryRecalls(message.execution_timeline),
    [message.execution_timeline],
  );
  const steps = useMemo(() => resolveSteps(message), [message]);
  const toolCalls = message.tool_calls ?? [];
  /** 工具执行统计：合并进消息头的统一 meta 行（与时间戳同排），
      不再单独占一行。折叠头的完整文案降级为 hover title。 */
  const toolSummary = (() => {
    if (!toolCalls.length) return null;
    const done = toolCalls.filter((tc) => tc.status !== "running").length;
    const n = done || toolCalls.length;
    const d = formatDuration(message.durationMs);
    return d ? `已完成 ${n} 个工具 ${d}` : `已完成 ${n} 个工具`;
  })();
  /** True when there's at least one tool still in "running" state.
   *  Prefer the cross-message signal from ChatView (`hasRunningToolProp`)
   *  when provided: it sees the streamingAssistantId + pendingToolsCount
   *  + hasPendingApproval union, which catches "LLM stream just settled
   *  but the WS session is still active" — the local toolCalls check
   *  below cannot. Fall back to the local derivation for non-ChatView
   *  callers (tests, previews). */
  const hasRunningTool = hasRunningToolProp ?? useMemo(
    () => toolCalls.some((tc) => tc.status === "running" || !tc.status),
    [toolCalls],
  );

  // Path clicks route through ChatView's `onPathClick` → RightSidebar
  // (never an inline popover) so the preview stays in the same column as
  // the chat instead of falling below it.

  return (
    // Outer wrapper: with header we use the avatar+column flex; without
    // header we keep the left margin (so body text aligns with the first
    // assistant message) but skip the avatar column and the header row.
    <div className={showHeader ? "flex gap-2.5 h-enter items-start msg-copyable" : "h-enter msg-copyable"}>
      {showHeader && (
        <Avatar role="assistant" size={22} className="shrink-0 mt-0.5" />
      )}
      <div className={showHeader ? "flex-1 min-w-0" : "pl-[30px]"}>
        {/* Header: time · model · stop chip. Copy belongs below the
            assistant message, not in this metadata line, so the action
            remains spatially attached to the content it copies. */}
        {showHeader && (
          <div className="flex items-center gap-2 mb-1.5 text-text-faint">
            <span className="h-tiny tabular-nums">{formatRelativeTime(message.timestamp)}</span>
            {message.model && <span className="h-tiny opacity-60">·</span>}
            {message.model && <span className="h-tiny">{message.model}</span>}
            {toolSummary && <span className="h-tiny opacity-60">·</span>}
            {toolSummary && <span className="h-tiny">{toolSummary}</span>}
            {message.stopReason && <StopReasonChip reason={message.stopReason} meta={message.stopReasonMeta} />}
            {message.compressed && (
              <span
                className="h-tiny px-1.5 rounded-sm"
                style={{
                  background: "var(--color-bg-tertiary, rgba(255,255,255,0.06))",
                  color: "var(--color-text-secondary)",
                  fontSize: 10,
                }}
              >
                已压缩
              </span>
            )}
          </div>
        )}

        {/* Unified timeline steps — renders reasoning / tools / text
            in chronological order so multi-iteration agent loops
            display each step as a cohesive unit instead of a fixed
            3-section layout (all reasoning → all tools → all text).

            Replaces the previous layout where
            ReasoningBlock, ToolTimelineSection, and the body card
            were always rendered in that fixed order regardless of
            the actual chronological flow. */}
        <UnifiedTimelineRenderer
          steps={steps}
          isStreaming={isStreaming}
          keepOpen={keepOpen}
          onPathClick={onPathClick}
          fileChanges={fileChanges}
          memoryUpdates={memoryUpdates}
          memoryRecalls={memoryRecalls}
          toolCalls={toolCalls}
          hasRunningTool={hasRunningTool}
          durationMs={message.durationMs}
          onApproveApproval={onApproveApproval}
          onDenyApproval={onDenyApproval}
        />

        {/* Backend badge (outside card so it reads as meta, not part of answer) */}
        {message.execution_backend && (
          <div className="mt-2 px-1 h-tiny text-text-faint">
            {message.execution_backend === "sandbox" ? "沙箱执行" : "本地执行"}
          </div>
        )}

        <MessageCopyAction
          content={message.content}
          className="message-copy-row message-copy-row--assistant"
        />
      </div>

    </div>
  );
}
export function SystemMessage({ message }: { message: ChatMessage }) {
  const full = message.content ?? "";
  const PREVIEW = 200;
  const preview = full.length > PREVIEW ? full.slice(0, PREVIEW) + "…" : full;
  return (
    <div className="flex gap-2 items-center justify-center my-2 h-enter">
      <AlertCircle size={12} className="text-text-faint" />
      <span className="h-tiny italic text-text-faint">{preview}</span>
    </div>
  );
}
