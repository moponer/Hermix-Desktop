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
 * Phase prop from ChatView drives the static status label in the meta line:
 *   streaming  → "正在思考…" (static, no internal dot animation)
 *   slow       → muted ⚠️ + "响应较慢" (no red, no error tone)
 *   completed  → (meta line collapses — no chip, no time text)
 *   error      → muted gray "出错了"
 *   timeout    → muted yellow "超时"
 *   partial    → muted gray "未完成"
 *
 * Terminal states render no chip / timestamp (MessageBubble takes over
 * and shows the real time). In-progress states keep the "正在思考…" label
 * so the bubble still reads as "working", but we intentionally avoid extra
 * animated dots/shimmers inside the bubble — the single bottom
 * AgentActivityIndicator is the only motion cue.
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
import { MarkdownBody } from "./MarkdownBody";
import { parseContent } from "./messageParser";
import { ProcessTimeline, MessageCopyAction } from "./MessageBubble";
import { StreamingArtifactCard } from "./StreamingArtifactCard";
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
  /**
   * Keep the reasoning/tool block EXPANDED even when the stream phase
   * is not "streaming" — e.g. a tool is in flight between agent-loop
   * turns and the LLM stream has settled. Without this, the thinking
   * block collapses the moment the text stream settles mid-loop,
   * which makes it look like the turn ended when it has not. Only
   * the truly final, idle state should collapse. Driven by ChatView
   * from the session's pending-approval / running-tool signals;
   * defaults false.
   */
  keepOpen?: boolean;
}

/**
 * STATUS_MAP visual rules:
 *  - silent / terminal states (completed / idle) → render nothing
 *    (a "completed" label would be redundant noise)
 *  - failure states (error / timeout / partial) → minimal icon + text,
 *    no chip background
 *  - in-progress states (streaming / connecting / slow) → text only,
 *    no chip visual weight
 *  - all states share `h-tiny` sizing; colour comes from className,
 *    no pill-* green fill
 */
const STATUS_MAP: Record<StreamPhase, { icon: React.ReactNode; label: string; className: string }> = {
  streaming:    { icon: null,                                                                       label: "正在思考…", className: "text-text-faint italic" },
  connecting:   { icon: null,                                                                       label: "正在连接…", className: "text-text-faint italic" },
  slow:         { icon: <AlertTriangle aria-hidden="true" className="icon-sm text-text-warning" />,  label: "响应较慢",  className: "text-text-warning" },
  reconnecting: { icon: <AlertTriangle aria-hidden="true" className="icon-sm text-text-warning" />,  label: "正在重连…", className: "text-text-warning italic" },
  completed:    { icon: null,                                                                          label: "",          className: "" },
  error:        { icon: <XCircle aria-hidden="true" className="icon-sm text-text-secondary" />,        label: "出错了",     className: "text-text-secondary italic" },
  timeout:      { icon: <Clock aria-hidden="true" className="icon-sm text-text-warning" />,           label: "超时",      className: "text-text-warning italic" },
  partial:      { icon: <Clock aria-hidden="true" className="icon-sm text-text-secondary" />,         label: "未完成",    className: "text-text-secondary italic" },
  idle:         { icon: null, label: "", className: "" },
};

const STREAMING_PHASES: ReadonlySet<StreamPhase> = new Set<StreamPhase>([
  "streaming",
  "connecting",
  "slow",
  "reconnecting",
]);

/** Detect retry/error status messages that should show even during streaming phases. */
function isRetryStatus(statusText: string): boolean {
  return /retry|retrying|重试|invalid.*tool|truncated|empty.*response/i.test(statusText);
}

/**
 * Streaming reasoning row — always-expanded process prose.
 * replaced the per-row "已深度思考中… ▼" fold. The whole
 * process timeline is now wrapped in a single ProcessTimeline block
 * (one fold, not N folds), so this row just renders the content.
 * Kept exported in case future code paths reuse it; the live bubble
 * currently delegates to MessageBubble's <ProcessTimeline> directly.
 */
export function StreamingReasoningBlock({ content }: { content: string; isStreaming: boolean }) {
  // Strip both <think> and <reasoning> wrappers for the same reason as
  // MessageBubble's ReasoningBlock: the surrounding ProcessTimeline is the
  // visual marker, not the raw XML tag.
  const clean = content
    .replace(/<\/?think>/gi, "")
    .replace(/<\/?reasoning>/gi, "")
    .trim();
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
  keepOpen = false,
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

  // When the LLM hasn't sent any text yet (e.g. it just started and is
  // about to call a tool), the static header label "正在思考…" is still
  // visible, so we no longer need an additional animated placeholder.
  const isStreamingPhase = STREAMING_PHASES.has(effectivePhase);

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
      <div className={showHeader ? "flex-1 min-w-0" : "pl-thread"}>
        {/* Meta line: time + status. Copy is deliberately rendered
            below the message body, matching the settled assistant bubble
            and keeping the action attached to the content it copies. */}
        {showHeader && (
          <div className="flex items-center gap-2 mb-1.5 text-text-faint">
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
              <span role="status" aria-live="polite" className="h-tiny text-text-faint max-w-narrow truncate">
                {statusText}
              </span>
            )}
          </div>
        )}

        {/* 产物级进度：LLM 正在写哪些文件 / 写到哪了。
            仅在流式态显示，流结束后由 MessageBubble 接管。 */}
        <StreamingArtifactCard text={deferred} isStreaming={isStreamingPhase} />

        {/* Process timeline — one chronological stream where reasoning
            + tools collapse behind a single header and text stays
            inline at its true position (matches MessageBubble). During
            live streaming the fold is always expanded so the narrative
            reads in arrival order. */}
        {(() => {
          // Live: there's always at least one tool in-flight or about
          // to fire, otherwise the stream would be settled already.
          const hasRunningTool = isStreamingPhase && effectiveSteps.some(
            (s) => s.kind === "tools" && s.events.some(
              (e) => e.type === "tool_start" || e.type === "tool_stdout",
            ),
          );
          return (
            <div className="tl-iterations">
              {/* One chronological stream (live = always expanded):
                  reasoning + tools + text render inline in arrival
                  order, so a lead-in sentence sits above the tool card
                  it preceded. */}
              <ProcessTimeline
                steps={effectiveSteps}
                isStreaming={isStreamingPhase}
                keepOpen={keepOpen}
                hasRunningTool={hasRunningTool}
                /* durationMs is only displayed when the turn settles
                   (i.e. isStreaming=false), so during the live stream
                   we deliberately don't compute it. App.tsx stamps the
                   duration on the message at the Done event; the
                   settled MessageBubble picks it up on re-render. */
                onPathClick={onPathClick}
              />
            </div>
          );
        })()}

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
  prev.keepOpen === next.keepOpen &&
  prev.reasoningContent === next.reasoningContent &&
  prev.executionTimeline === next.executionTimeline &&
  prev.unifiedSteps === next.unifiedSteps,
);
