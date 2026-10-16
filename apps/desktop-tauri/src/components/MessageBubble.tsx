import React from "react";
import { Sparkles } from "lucide-react";
import type { ChatMessage } from "../types";
import {
  UserMessage, SystemMessage, AssistantMessage,
} from "./TextBubble";
import { ClarifyCard } from "./ClarifyBubble";
import { ToolMessageCard } from "./ToolCallBubble";
import { AutomationBubble } from "./AutomationBubble";

// Re-export so `import { ThoughtStreamCard } from "./MessageBubble"` keeps
// working.
export { ThoughtStreamCard } from "./ThoughtStreamCard";
// Exported for reuse in StreamingBubble — the live streaming bubble and the
// settled MessageBubble render the tool timeline from the same component, so
// visuals stay identical. ProcessTimeline is the canonical foldable process
// surface; ToolTimelineSection is the flat tool list it renders.
export { ToolTimelineSection, ProcessTimeline } from "./ToolCallBubble";
// Re-export so `import { MessageCopyAction } from "./MessageBubble"` keeps
// working (StreamingBubble reuses the same copy affordance).
export { MessageCopyAction } from "./MessageCopyAction";
interface MessageBubbleProps {
  message: ChatMessage;
  /** Workspace root used to anchor relative paths the LLM mentions. */
  workspaceRoot?: string | null;
  /**
   * Set true only when this is the actively-streaming assistant message.
   * Gates the "等待回复" (waiting for final reply) placeholder so it
   * only fires on the live turn — never on historical/reloaded turns
   * that happen to have empty body + completed tools. See
   * `AssistantMessage` for the full reasoning.
   */
  isStreaming?: boolean;
  /**
   * Keep the reasoning/tool block EXPANDED even when `isStreaming` is
   * false — e.g. the agent loop is paused on a pending approval card or
   * a long-running tool and has not produced its final output yet.
   * Without this, the thinking block collapses the moment the text
   * stream settles (WS closes while awaiting approval), which makes it
   * look like the turn finished when it has not. Only the truly final,
   * idle state should collapse. Driven by ChatView from the session's
   * pending-approval / running-tool signals; defaults false.
   */
  keepOpen?: boolean;
  onApproveApproval?: (requestId: string, remember?: boolean) => void;
  onDenyApproval?: (requestId: string) => void;
  onApproveBatchApproval?: (batchId: string, remember?: boolean) => void;
  onDenyBatchApproval?: (batchId: string) => void;
  onReasoningFoldChange?: (folded: boolean) => void;
  /** 「重新生成」：重新发送上一条用户消息。父级不传就不渲染该按钮。 */
  onRetryLastMessage?: () => void;
  onAnswerClarify?: (requestId: string, answer: string) => void;
  /** Click handler for absolute/relative paths mentioned in tool
   *  rows and the file-change list.  Used to pop the inline file
   *  preview.  Only meaningful on top-level `role: "tool"` messages
   *  and assistant replies. */
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /**
   * Whether this assistant message should render the avatar + name +
   * metadata header. We show the header on the FIRST assistant
   * message in a turn (the one immediately after a user/tool input)
   * and hide it on continuation messages within the same turn. The
   * body content, reasoning block, and tool timeline keep rendering
   * either way — only the header chrome is suppressed. This avoids
   * the "avatar / name printed N times for one user question" feel
   * and matches the WorkBuddy pattern of one identification per turn.
   *
   * Defaults to `true` so non-ChatView callers (e.g. tests, isolated
   * previews) get the full visual treatment; ChatView explicitly
   * passes `false` for continuation assistant messages.
   */
  showHeader?: boolean;
  /**
   * Watershed timestamp (ms) for an expert switch this session: messages
   * before it render with the old expert's avatar/time, messages at/after
   * it with the new expert's. `null` means no switch happened.
   */
  expertCutoffAt?: number | null;
  /**
   * True when at least one tool call on this message is still running
   * in the background (between the LLM stream settling and the next
   * LLM call). Used by the expanded/collapsed decision for the
   * reasoning + tool timeline block: collapsing while a tool is still
   * in-flight makes it look like the turn ended when it has not. The
   * companion `keepOpen` prop covers the explicit "paused for approval"
   * case; this one covers the implicit "background tool running" /
   * "streaming session still active" case. Driven by ChatView from
   * `pendingToolsCount` and `streamingAssistantId`. Defaults to false
   * so non-ChatView callers (e.g. tests) keep the legacy auto-collapse.
   */
  hasRunningTool?: boolean;
}
export function MessageBubble({
  message, workspaceRoot,
  onApproveApproval, onDenyApproval, onAnswerClarify,
  onApproveBatchApproval, onDenyBatchApproval,
  onReasoningFoldChange,
  onRetryLastMessage,
  onPathClick,
  showHeader = true,
  isStreaming = false,
  keepOpen = false,
  hasRunningTool = false,
  expertCutoffAt,
}: MessageBubbleProps) {
  if (message.role === "user") return <UserMessage message={message} />;
  if (message.role === "system") return <SystemMessage message={message} />;
  if (message.role === "approval") {
    // 审批消息一律不内联渲染（T7：原写法是 if(decision) return null;
    // return null; 的冗余双分支，行为等价，简化为单 return）——
    // Pending 态由 ChatView 底部的 ApprovalDrawer（输入框上方抽屉）
    // 统一承载，避免同一请求出现两处可点击的决策面；已决议的不在
    // 消息流留痕（决议结果体现在工具卡的 approval-pill 上）。
    return null;
  }
  if (message.role === "clarify") {
    // 已答复的询问卡直接消失，不留"已答复"占位（与审批卡一致）。
    if (message.clarify?.answer) {
      return null;
    }
    return (
      <ClarifyCard
        message={message}
        onAnswer={onAnswerClarify}
      />
    );
  }
  if (message.role === "tool") {
    return (
      <ToolMessageCard
        message={message}
        workspaceRoot={workspaceRoot}
        onPathClick={onPathClick}
      />
    );
  }
  // Must sit above the assistant fallback: this role is neither, and
  // falling through would render a scheduler result as if the model
  // had said it.
  if (message.role === "automation") {
    return <AutomationBubble message={message} />;
  }
  return (
    <AssistantMessage
      message={message}
      workspaceRoot={workspaceRoot}
      showHeader={showHeader}
      isStreaming={isStreaming}
      keepOpen={keepOpen}
      hasRunningTool={hasRunningTool}
      expertCutoffAt={expertCutoffAt}
      onApproveApproval={onApproveApproval}
      onDenyApproval={onDenyApproval}
      onApproveBatchApproval={onApproveBatchApproval}
      onDenyBatchApproval={onDenyBatchApproval}
      onReasoningFoldChange={onReasoningFoldChange}
        onRetryLastMessage={onRetryLastMessage}
      // AssistantMessage 也要拿到 onPathClick：漏传会让恢复态（重载后的
      // 历史消息）正文里的路径链接点击变成 no-op —— 流式当轮走
      // StreamingBubble 那条链是好的，重载后走这条链，点击侧栏不开预览。
      onPathClick={onPathClick}
    />
  );
}
interface PrefetchIntentChipProps {
  intent: string;
  confidence: number;
}

export function PrefetchIntentChip({ intent, confidence }: PrefetchIntentChipProps) {
  const pct = Math.round(confidence * 100);
  return (
    <div className="prefetch-chip h-enter" title={`意图预判：${intent}`}>
      <div className="prefetch-dot" />
      <Sparkles className="icon-xs text-success" />
      <span className="prefetch-label">预取</span>
      <span className="prefetch-label" style={{ color: "var(--color-text-primary)" }}>{intent}</span>
      <span className="prefetch-confidence">{pct}%</span>
    </div>
  );
}
