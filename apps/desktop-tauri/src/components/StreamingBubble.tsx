/**
 * StreamingBubble — slim bubble used during the "正在思考" phase
 * of an assistant turn.
 *
 * Visual contract: this component is *layout-equivalent* to a
 * MessageBubble with `showHeader=true`. The same Avatar + meta
 * line (time + status + copy button) is rendered on top of
 * the markdown body, so when the stream settles and the
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
 * 2026-06-17 主人反馈 "已完成几个字的设计难看"：
 *   终结态不再显示任何 chip / 时间戳 (因为 MessageBubble 会接管并显示真实时间).
 *   进行态依然有 "正在生成…" 文字 + 段间隙动画, 不变.
 *
 * 段间隙动画（2026-06-12 主人需求）：
 *   "正在生成…"的动画从 header 移到段间隙位置——每段输出结束后、下一段
 *   还没开始时，在 markdown 内容末尾的下一行行首显示心跳 + 扫描线。
 *   检测 markdown 文本是否以换行符结尾：`/\n\s*$/`。
 *   · 文本以换行结尾 → 刚结束一段、下一段还没来 → 显示 dot
 *   · 文本不以换行结尾 → 当前段还在写 → 不显示 dot（避免段内重叠）
 *   · 流结束（phase !== streaming/connecting/slow） → 自动消失
 */

import { memo, useDeferredValue, useMemo, useState } from "react";
import { Check, Copy, AlertTriangle, XCircle, Clock } from "lucide-react";
import { Avatar } from "./Avatar";
import { InlineThinkingDot } from "./InlineThinkingDot";
import { MarkdownBody } from "./MarkdownBody";
import { parseContent } from "./messageParser";
import type { StreamPhase } from "../hooks/useStreamChat";

/**
 * 头部时间戳规则 (2026-06-17 主人反馈 "已完成几个字的设计难看"):
 *  - 进行态 (streaming / connecting / slow) → "正在生成…"
 *  - 终结态 (completed / idle / error / timeout / partial) → ""
 *
 * 流式气泡里不显示绝对时间 (那是 MessageBubble 的事), 也再没有 "刚刚/已完成"
 * 这种重复标签.
 */
function formatRelativeTimeText(phase: StreamPhase): string {
  switch (phase) {
    case "streaming":
    case "connecting":
    case "slow":
    case "reconnecting":
      return "正在生成…";
    default:
      return "";
  }
}

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
   * Whether to render the avatar + meta line (time / status / copy).
   * Must come from the same `shouldShowAssistantHeader` logic that
   * `<MemoMessageBubble showHeader={...} />` uses — otherwise the
   * streaming→settled transition would *add or remove* the header
   * mid-turn, which is the "avatar appears twice in one agent loop"
   * bug.
   *
   * v18 (2026-06-17): multiple streaming placeholders within a single
   * agent loop used to each render their own header because this
   * component hard-coded `showHeader=true`. Now the parent decides
   * — true only for the first assistant placeholder after a user
   * message, false for every subsequent one (whether the LLM settled
   * and started a new turn, or a tool cleared the stream and a fresh
   * placeholder was created).
   */
  showHeader?: boolean;
}

/**
 * STATUS_MAP 视觉规则 (2026-06-17 主人反馈 "已完成几个字的设计难看"):
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

const NOOP = () => {};

function StreamingBubbleImpl({
  text,
  phase = "streaming",
  onPathClick,
  statusText,
  showHeader = true,
}: StreamingBubbleProps) {
  const deferred = useDeferredValue(text);
  const effectivePhase = phase ?? "streaming";
  const [copied, setCopied] = useState(false);
  const copyMessage = async () => {
    await navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  const status = STATUS_MAP[effectivePhase];

  // Strip infrastructure that the LLM should see but the user shouldn't
  // (CONTEXT COMPACTION banner, <minimax:tool_call> XML leaks, <think>
  // blocks, [lifecycle:…] noise, checkbox tasks). MessageBubble runs the
  // same `parseContent` on persisted messages — but StreamingBubble is
  // mounted BEFORE the stream settles and renders raw streamed chunks
  // directly through MarkdownBody. Without this parse, the user sees
  // the [CONTEXT COMPACTION] English wall whenever the model echoes the
  // banner mid-stream (verified 2026-06-27: banner echo leaked 1.2KB
  // English text into the chat despite MessageBubble's strip working
  // post-stream).
  //
  // Performance: parseContent is O(text-length) and we only feed the
  // already-`useDeferredValue`-coalesced `deferred` to it, so we don't
  // pay more than one parse per animation frame regardless of how many
  // WebSocket chunks arrived.
  const parsedStream = useMemo(() => parseContent(deferred), [deferred]);

  const body = useMemo(
    () => (
      <MarkdownBody
        content={parsedStream.body}
        onPathClick={onPathClick ?? NOOP}
      />
    ),
    [parsedStream.body, onPathClick],
  );

  const showGapDot =
    STREAMING_PHASES.has(effectivePhase) && endsAtParagraphBoundary(deferred);

  // When the LLM hasn't sent any text yet (e.g. it just started and is
  // about to call a tool), show a "thinking…" placeholder so the
  // avatar + animation is always visible during a turn.
  const isStreamingPhase = STREAMING_PHASES.has(effectivePhase);
  const hasNoContent = !parsedStream.body || parsedStream.body.trim().length === 0;

  return (
    // Same outer layout as MessageBubble(showHeader=true) so the
    // avatar + meta line above the body matches the post-stream
    // bubble. No "header pops in" jump when stream settles.
    //
    // v18 (2026-06-17): when `showHeader=false` we still need to
    // preserve the left padding so the body text aligns with the
    // FIRST assistant message in the loop (which has the header).
    // We mirror MessageBubble's pattern: header→flex with avatar
    // column; no header→just a left padding matching the avatar
    // column width.
    <div className={showHeader ? "flex gap-2.5 h-enter items-start" : "h-enter"}>
      {showHeader && (
        <Avatar role="assistant" size={22} className="shrink-0 mt-0.5" />
      )}
      <div className={showHeader ? "flex-1 min-w-0" : "pl-[30px]"}>
        {/* Meta line: time + status + copy button.
            v12 (2026-06-17): completed/idle 状态不再渲染任何 chip,
            "已完成" / "刚刚" 这种废话标签直接消失. 仅保留进行态和失败态.
            v18 (2026-06-17): 整个 meta 行受 `showHeader` 控制 — 同一个
            agent loop 内的多轮 streaming 只在第一轮显示, 后续轮沿用
            第一个 assistant 的 header, 避免"刚刚 + 头像 + 名称"重复
            出现. */}
        {showHeader && (
          <div className="flex items-center gap-1.5 mb-1 text-text-faint">
            <span className="h-tiny">{formatRelativeTimeText(effectivePhase)}</span>
            {status?.icon}
            {status?.label && (
              <span className={`h-tiny ${status?.className ?? ""}`}>{status?.label}</span>
            )}
            {statusText && (
              <span className="h-tiny text-text-faint italic max-w-[240px] truncate">
                {statusText}
              </span>
            )}
            <button
              onClick={copyMessage}
              disabled={!text}
              className="ml-auto p-0.5 rounded text-text-faint hover:text-text-primary transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
              title="复制消息"
            >
              {copied ? <Check size={11} /> : <Copy size={11} />}
            </button>
          </div>
        )}

        {hasNoContent && isStreamingPhase ? (
          showHeader ? (
            <InlineThinkingDot
              hint={
                effectivePhase === "slow"
                  ? "⚠️ 响应较慢"
                  : effectivePhase === "connecting"
                  ? "正在连接"
                  : "正在思考…"
              }
            />
          ) : (
            // No header → no "正在思考…" pill. Per v18, a
            // continuation placeholder within the same loop just
            // appears as empty space (the post-stream MessageBubble
            // will materialise the body content here). Showing a
            // floating "正在思考…" pill on continuation rounds was
            // the source of the redundant-status feedback.
            null
          )
        ) : (
          <>
            {body}
            {showGapDot && (
              <InlineThinkingDot
                asGap
                hint={
                  effectivePhase === "slow"
                    ? "⚠️ 响应较慢"
                    : effectivePhase === "connecting"
                    ? "正在连接"
                    : "正在思考…"
                }
              />
            )}
          </>
        )}
      </div>
    </div>
  );
}

export const StreamingBubble = memo(StreamingBubbleImpl, (prev, next) =>
  prev.text === next.text &&
  prev.phase === next.phase &&
  prev.onPathClick === next.onPathClick &&
  prev.statusText === next.statusText &&
  prev.showHeader === next.showHeader,
);
