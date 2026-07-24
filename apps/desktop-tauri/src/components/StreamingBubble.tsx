/**
 * StreamingBubble — slim bubble used during the "正在思考" phase
 * of an assistant turn.
 *
 * Visual contract: this component is *layout-equivalent* to a
 * MessageBubble with `showHeader=true`. The same Avatar + meta
 * line (time + status + copy button) is rendered on top of the
 * markdown body, so when the stream settles and the
 * StreamingBubble is swapped for a MessageBubble the user does
 * not see a "header pops in" jump. Body rendering is also shared
 * (MarkdownBody) so a fenced code block in a streaming reply
 * shows the language label + copy button + oneDark highlight the
 * moment the closing ``` arrives.
 *
 * Phase prop from ChatView drives the status label and the gap
 * animation:
 *   streaming  → "正在生成…" + 段间隙动画
 *   slow       → muted ⚠️ + "响应较慢" (no red, no error tone)
 *   completed  → (meta line collapses — no chip, no time text)
 *   error      → muted gray "出错了"
 *   timeout    → muted yellow "超时"
 *   partial    → muted gray "未完成"
 *
 * 用户反馈 "已完成几个字的设计难看"：
 *   终结态不再显示任何 chip / 时间戳 (因为 MessageBubble 会接管并显示真实时间).
 *   进行态依然有 "正在生成…" 文字 + 段间隙动画, 不变.
 *
 * 段间隙动画（用户需求）：
 *   "正在生成…"的动画从 header 移到段间隙位置——每段输出结束后、下一段
 *   还没开始时，在 markdown 内容末尾的下一行行首显示心跳 + 扫描线。
 *   检测 markdown 文本是否以换行符结尾：`/\n\s*$/`。
 *   · 文本以换行结尾 → 刚结束一段、下一段还没来 → 显示 dot
 *   · 文本不以换行结尾 → 当前段还在写 → 不显示 dot（避免段内重叠）
 *   · 流结束（phase !== streaming/connecting/slow） → 自动消失
 *
 * Unified timeline rendering — reasoning / tools / text
 * are now rendered in chronological order using `unifiedSteps`,
 * matching the settled MessageBubble's layout. This ensures that
 * multi-iteration agent loops (think → tool → think → tool → text)
 * display each step as a cohesive unit instead of a fixed 3-section
 * layout.
 */

import { Clock } from "lucide-react";
import { memo, useDeferredValue, useMemo } from "react";
import { AlertTriangle, XCircle } from "lucide-react";
import { Avatar } from "./Avatar";
import { InlineThinkingDot } from "./InlineThinkingDot";
import { MarkdownBody } from "./MarkdownBody";
import { parseContent } from "./messageParser";
import { ProcessTimeline, MessageCopyAction } from "./MessageBubble";
import type { StreamPhase } from "../hooks/useStreamChat";
import type { ExecutionTimelineEvent, TimelineStep } from "../types";

interface StreamingBubbleProps {
  text: string;
  phase?: StreamPhase | null;
  /** Path click handler — passed through to MarkdownBody. If omitted,
   *  path links are still rendered but clicking them is a no-op. */
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /** Transient status text from backend (e.g. "Still waiting for the model…").
   *  Shown as a muted chip in the meta line, NOT as message text. */
  statusText?: string | null;
  /**
   * Whether to render the avatar + meta line (time / status).
   * The copy action is rendered below the streamed message body so the
   * live bubble and settled bubble keep the same spatial structure.
   * Must come from the same `shouldShowAssistantHeader` logic that
   * `<MemoMessageBubble showHeader={...} />` uses — otherwise the
   * streaming→settled transition would *add or remove* the header
   * mid-turn, which is the "avatar appears twice in one agent loop"
   * bug.
   *
   * multiple streaming placeholders within a single
   * agent loop used to each render their own header because this
   * component hard-coded `showHeader=true`. Now the parent decides
   * — true only for the first assistant placeholder after a user
   * message, false for every subsequent one (whether the LLM settled
   * and started a new turn, or a tool cleared the stream and a fresh
   * placeholder was created).
   */
  showHeader?: boolean;
  /** Live reasoning text from the LLM (<think>...</think> block).
   *  Retained for backward compat — when `unifiedSteps` is provided,
   *  this is not used. */
  reasoningContent?: string;
  /** Live tool execution timeline from the assistant's
   *  execution_timeline. Rendered as a collapsible block inside the
   *  streaming bubble, same as in the settled MessageBubble.
   *  Retained for backward compat — when `unifiedSteps` is provided,
   *  this is not used. */
  executionTimeline?: ExecutionTimelineEvent[];
  /** Unified timeline steps — chronological sequence of
   *  reasoning / tools / text. When provided, takes priority over
   *  the legacy `reasoningContent` + `executionTimeline` + `text`
   *  trio. */
  unifiedSteps?: TimelineStep[];
}

/**
 * STATUS_MAP 视觉规则 (用户反馈 "已完成几个字的设计难看"):
 *  - 静默 / 终结态 (completed / idle) → render 空 (不显示, 已完成是废话)
 *  - 失败态 (error / timeout / partial) → 极简图标 + 文字, 不用任何 chip 背景
 *  - 进行态 (streaming / connecting / slow) → 文字, 不带 chip 视觉重量
 *  - 所有状态共用 `h-tiny` 字号, 颜色由 className 控制, 不再有 pill-* 绿底
 */
const STATUS_MAP: Record<StreamPhase, { icon: React.ReactNode; label: string; className: string }> = {
  streaming:    { icon: null,                                                                       label: "正在思考…", className: "text-text-faint italic" },
  connecting:   { icon: null,                                                                       label: "正在连接…", className: "text-text-faint italic" },
  slow:         { icon: <AlertTriangle size={11} className="text-text-warning" />,                  label: "响应较慢",  className: "text-text-warning" },
  reconnecting: { icon: <AlertTriangle size={11} className="text-text-warning" />,                  label: "正在重连…", className: "text-text-warning italic" },
  completed:    { icon: null,                                                                       label: "",          className: "" },
  error:        { icon: <XCircle size={11} className="text-text-secondary" />,                     label: "出错了",     className: "text-text-secondary italic" },
  timeout:      { icon: <Clock size={11} className="text-text-warning" />,                         label: "超时",      className: "text-text-warning italic" },
  partial:      { icon: <Clock size={11} className="text-text-secondary" />,                       label: "未完成",    className: "text-text-secondary italic" },
  idle:         { icon: null, label: "", className: "" },
};

const STREAMING_PHASES: ReadonlySet<StreamPhase> = new Set<StreamPhase>([
  "streaming",
  "connecting",
  "slow",
  "reconnecting",
]);

/**
 * 检测 markdown 文本是否"刚结束一段、下一段还没开始"：
 *   · 末尾是单个 `\n` 或两个 `\n`（paragraph break）→ true
 *   · 末尾是空白 + 换行                              → true
 *   · 其他（当前段还在写、或完全空文本）              → false
 */
function endsAtParagraphBoundary(text: string): boolean {
  if (text.length === 0) return false;
  return /\n\s*$/.test(text);
}

/** Detect retry/error status messages that should show even during streaming phases. */
function isRetryStatus(statusText: string): boolean {
  return /retry|retrying|重试|invalid.*tool|truncated|empty.*response/i.test(statusText);
}

const NOOP = () => {};

/**
 * Streaming reasoning row — always-expanded process prose.
 * replaced the per-row "已深度思考中… ▼" fold. The whole
 * process timeline is now wrapped in a single ProcessTimeline block
 * (one fold, not N folds), so this row just renders the content.
 * Kept exported in case future code paths reuse it; the live bubble
 * currently delegates to MessageBubble's <ProcessTimeline> directly.
 */
export function StreamingReasoningBlock({ content }: { content: string; isStreaming: boolean }) {
  const clean = content.replace(/<\/?think>/gi, "").trim();
  if (!clean) return null;
  return (
    <div className="tl-reasoning-row">
      <MarkdownBody content={clean} onPathClick={() => {}} />
    </div>
  );
}

function StreamingBubbleImpl({
  text,
  phase = "streaming",
  onPathClick,
  statusText,
  showHeader = true,
  reasoningContent,
  executionTimeline,
  unifiedSteps,
}: StreamingBubbleProps) {
  const deferred = useDeferredValue(text);
  const effectivePhase = phase ?? "streaming";

  const status = STATUS_MAP[effectivePhase];

  // Strip infrastructure that the LLM should see but the user shouldn't
  // (CONTEXT COMPACTION banner, <minimax:tool_call> XML leaks, <think>
  // blocks, [lifecycle:…] noise, checkbox tasks). MessageBubble runs the
  // same `parseContent` on persisted messages — but StreamingBubble is
  // mounted BEFORE the stream settles and renders raw streamed chunks
  // directly through MarkdownBody. Without this parse, the user sees
  // the [CONTEXT COMPACTION] English wall whenever the model echoes the
  // banner mid-stream (verified banner echo leaked 1.2KB
  // English text into the chat despite MessageBubble's strip working
  // post-stream).
  const parsedStream = useMemo(() => parseContent(deferred), [deferred]);

  // Build effective steps: use unifiedSteps when available, otherwise
  // fall back to the legacy 3-field approach.
  const effectiveSteps: TimelineStep[] = useMemo(() => {
    if (unifiedSteps && unifiedSteps.length > 0) return unifiedSteps;
    const steps: TimelineStep[] = [];
    const reasoning = (reasoningContent ?? "").replace(/<\/?think>/gi, "").trim();
    if (reasoning) steps.push({ kind: "reasoning", content: reasoning });
    const toolEvents = (executionTimeline ?? []).filter(
      (e) => e.type === "tool_start" || e.type === "tool_stdout" || e.type === "tool_complete",
    );
    if (toolEvents.length > 0) steps.push({ kind: "tools", events: toolEvents });
    if (parsedStream.body && parsedStream.body.trim()) steps.push({ kind: "text", content: parsedStream.body });
    return steps;
  }, [unifiedSteps, reasoningContent, executionTimeline, parsedStream.body]);

  const showGapDot =
    STREAMING_PHASES.has(effectivePhase) && endsAtParagraphBoundary(deferred);

  // When the LLM hasn't sent any text yet (e.g. it just started and is
  // about to call a tool), show a "thinking…" placeholder so the
  // avatar + animation is always visible during a turn.
  const isStreamingPhase = STREAMING_PHASES.has(effectivePhase);
  const hasTextStep = effectiveSteps.some((s) => s.kind === "text");
  const hasNoContent = !hasTextStep;

  // detect whether any tool group is still running so we can
  // suppress the shimmer indicator while tool cards are active.
  const hasRunningToolInSteps = isStreamingPhase && effectiveSteps.some(
    (s) => s.kind === "tools" && s.events.some(
      (e) => e.type === "tool_start" || e.type === "tool_stdout",
    ),
  );

  return (
    // Same outer layout as MessageBubble(showHeader=true) so the
    // avatar + meta line above the body matches the post-stream
    // bubble. No "header pops in" jump when stream settles.
    // when `showHeader=false` we still need to
    // preserve the left padding so the body text aligns with the
    // FIRST assistant message in the loop (which has the header).
    // We mirror MessageBubble's pattern: header→flex with avatar
    // column; no header→just a left padding matching the avatar
    // column width.
    <div className={showHeader ? "flex gap-2.5 h-enter items-start msg-copyable" : "h-enter msg-copyable"}>
      {showHeader && (
        <Avatar role="assistant" size={22} className="shrink-0 mt-0.5" />
      )}
      <div className={showHeader ? "flex-1 min-w-0" : "pl-[30px]"}>
        {/* Meta line: time + status. Copy is deliberately rendered
            below the message body, matching the settled assistant bubble
            and keeping the action attached to the content it copies. */}
        {showHeader && (
          <div className="flex items-center gap-2 mb-1.5 text-text-faint">
            {STREAMING_PHASES.has(effectivePhase) && (
              <InlineThinkingDot
                size="sm"
                variant={effectivePhase === "slow" || effectivePhase === "reconnecting" ? "warning" : "default"}
                hideHint
              />
            )}
            {status?.icon}
            {status?.label && (
              <span className={`h-tiny ${status?.className ?? ""}`}>{status?.label}</span>
            )}
            {/* 流式态下中文 phase label（如"正在思考…"）已表达"等待中"，
                后端瞬态英文 statusText（"Still waiting for the model…"）属冗余
                且破坏中文 UI 一致性，故流式态抑制、仅非流式态展示。
                例外：重试/错误状态（retrying、invalid tool、truncated）
                传达了关键诊断信息，即使在流式态也应展示。 */}
            {statusText && (!STREAMING_PHASES.has(effectivePhase) || isRetryStatus(statusText)) && (
              <span className="h-tiny text-text-faint max-w-[240px] truncate">
                {statusText}
              </span>
            )}
          </div>
        )}

        {/* Process timeline (single foldable block) + final text prose.
            mirrors MessageBubble's new layout — every
            reasoning/tool step goes inside one ProcessTimeline fold,
            and text steps render as continuous prose below the fold.
            This matches the WorkBuddy reference UI: one "处理中… /
            已完成" header per turn, with the conclusion visible without
            a click. */}
        {(() => {
          const processSteps = effectiveSteps.filter(
            (s) => s.kind === "reasoning" || s.kind === "tools",
          );
          const textSteps = effectiveSteps.filter((s) => s.kind === "text");
          // Live: there's always at least one tool in-flight or about
          // to fire, otherwise the stream would be settled already.
          const hasRunningTool = isStreamingPhase && processSteps.some(
            (s) => s.kind === "tools" && s.events.some(
              (e) => e.type === "tool_start" || e.type === "tool_stdout",
            ),
          );
          return (
            <div className="tl-iterations">
              <ProcessTimeline
                processSteps={processSteps}
                isStreaming={isStreamingPhase}
                hasRunningTool={hasRunningTool}
                /* durationMs is only displayed when the turn settles
                   (i.e. isStreaming=false), so during the live stream
                   we deliberately don't compute it. App.tsx stamps the
                   duration on the message at the Done event; the
                   settled MessageBubble picks it up on re-render. */
                onPathClick={onPathClick}
              />
              {textSteps.map((step, i) => {
                if (step.kind !== "text") return null;
                const parsed = parseContent(step.content);
                return (
                  <div key={`text-${i}`} className="tl-iteration tl-text-step">
                    {parsed.body && (
                      <div className="tl-text-inner">
                        <MarkdownBody
                          content={parsed.body}
                          onPathClick={onPathClick ?? NOOP}
                        />
                      </div>
                    )}
                  </div>
                );
              })}
            </div>
          );
        })()}

        {/* Gap dot at the end of the last text step */}
        {showGapDot && (
          <InlineThinkingDot
            asGap
            variant={effectivePhase === "slow" || effectivePhase === "reconnecting" ? "warning" : "default"}
            hideHint
          />
        )}

        {/* Thinking placeholder when no content yet */}
        {hasNoContent && isStreamingPhase && (
          showHeader ? (
            <div className="flex items-center gap-2 py-3 text-text-muted">
              <InlineThinkingDot
                variant={effectivePhase === "slow" || effectivePhase === "reconnecting" ? "warning" : "default"}
                hideHint
              />
            </div>
          ) : null
        )}

        {/* "正在回复" 流光指示器 — 仅当 body 已有内容但流仍活跃时显示，
             让用户知道 agent 在持续思考/调工具而没有结束。
             严格边界：此组件只挂在 StreamingBubble（isStreaming=true）内，
             历史消息重载时根本不渲染 — 满足用户"恢复历史不显示"的要求。
             至少要有一段 reasoning 才挂，避免和上面"无内容 placeholder"重复。
             当有工具卡片正在运行时，隐藏 shimmer 指示器——
             工具卡片的 🔄 运行中 + 进度条已经是足够的状态指示，
             shimmer 的"正在回复"会在工具卡片下方制造冗余动画，
             让用户误以为"什么都没在做"。工具运行中时只保留工具卡片，
             工具完成后再恢复 shimmer。 */}
        {!hasNoContent && isStreamingPhase && !hasRunningToolInSteps &&
         ((unifiedSteps && unifiedSteps.length > 0)
            ? unifiedSteps.some((s) => s.kind === "reasoning")
            : (reasoningContent ?? "").trim().length > 0) && (
          <div className="flex items-center py-1.5 text-text-muted" data-testid="live-shimmer-indicator">
            <InlineThinkingDot
              variant={effectivePhase === "slow" || effectivePhase === "reconnecting" ? "warning" : "shimmer"}
              hint={effectivePhase === "slow" ? "响应较慢" : effectivePhase === "reconnecting" ? "正在重连" : "正在回复"}
            />
          </div>
        )}

        <MessageCopyAction
          content={text}
          className="message-copy-row message-copy-row--assistant"
        />
      </div>
    </div>
  );
}

export const StreamingBubble = memo(StreamingBubbleImpl, (prev, next) =>
  prev.text === next.text &&
  prev.phase === next.phase &&
  prev.onPathClick === next.onPathClick &&
  prev.statusText === next.statusText &&
  prev.showHeader === next.showHeader &&
  prev.reasoningContent === next.reasoningContent &&
  prev.executionTimeline === next.executionTimeline &&
  prev.unifiedSteps === next.unifiedSteps,
);
