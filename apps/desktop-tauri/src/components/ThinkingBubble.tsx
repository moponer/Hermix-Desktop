// Thinking/processing indicators previously colocated in MessageBubble.tsx.
import { useEffect, useMemo, useState } from "react";
import { Brain, ChevronRight, ChevronDown } from "lucide-react";
import { MarkdownBody } from "./MarkdownBody";
import { formatDuration } from "./bubbleUtils";

export function ThinkingDots({ label = "正在处理" }: { label?: string }) {
  return (
    <div className="tdots h-enter">
      <span className="tdots-label">{label}</span>
      <span className="tdots-dot" style={{ animationDelay: "0ms" }} />
      <span className="tdots-dot" style={{ animationDelay: "200ms" }} />
      <span className="tdots-dot" style={{ animationDelay: "400ms" }} />
    </div>
  );
}

/** 单段思考的墙钟耗时。恢复态（重载历史）没有时间戳 → undefined。 */
function spanMs(startedAt?: string, endedAt?: string): number | undefined {
  if (!startedAt) return undefined;
  const t0 = new Date(startedAt).getTime();
  // 没有 endedAt = 还在想，此刻为终点。
  const t1 = endedAt ? new Date(endedAt).getTime() : Date.now();
  if (Number.isNaN(t0) || Number.isNaN(t1)) return undefined;
  return Math.max(0, t1 - t0);
}

/**
 * 一段思考 = 一个**独立**的折叠项。
 *
 * 2026-10-01 用户实机走查后的定调（推翻此前"一轮一个总开关"的做法）：
 *
 * - **正在生成的那段展开，刚结束的那段自动收起。** 一轮里往往有多段
 *   思考，被工具调用隔开，各自是独立的一行，不是同一块里的段落。
 *   `active` 由外层判定（本段是否是 steps 数组的尾巴且仍在流）。
 * - 展开态的正文仍是旁注形态：12px 缩进 + 轨道 + 更浅的字色。
 *   「这是旁注」的标记靠**几何 + 文字标签**，不靠色差 —— 深色下正文
 *   15.32:1、思考 4.77:1 都过 AA，而能承载语义的细线旧值只有
 *   1.37:1，比它标记的正文还弱。
 * - 标签同时回答两个问题：这是什么（思考）、花了多久（持续了 N 秒）。
 *   收起的思考因此不会无声消失 —— 用户知道这里有内容没看。
 */
export function ReasoningBlock({
  content,
  total = 1,
  active = false,
  startedAt,
  endedAt,
}: {
  content: string;
  /** 本轮共几段思考 —— >1 时标签补一个段数。 */
  total?: number;
  /** 本段是否正在流式生成。 */
  active?: boolean;
  startedAt?: string;
  endedAt?: string;
}) {
  // Strip both <think> and <reasoning> wrappers — providers use both tags
  // for internal monologue, and the surrounding ProcessTimeline already
  // signals that this is process content.
  const clean = useMemo(
    () =>
      content
        .replace(/<\/?think>/gi, "")
        .replace(/<\/?reasoning>/gi, "")
        .trim(),
    [content],
  );

  // 正在想的那段自动展开；一旦本段结束（active 变 false）自动收起。
  // 用户手动点过之后就以用户的为准。
  const [pinned, setPinned] = useState<boolean | null>(null);
  useEffect(() => setPinned(null), []);
  const expanded = pinned ?? active;

  const elapsed = spanMs(startedAt, endedAt);
  const label = active
    ? "思考中…"
    : elapsed != null
      ? `思考 · 持续了 ${formatDuration(elapsed)}`
      : "思考";

  if (!clean) return null;

  return (
    <div className="tl-reasoning-wrap" data-active={active ? "true" : undefined}>
      <button
        type="button"
        className="tl-reasoning-head"
        onClick={() => setPinned(!expanded)}
        aria-expanded={expanded}
        aria-label={label}
      >
        {expanded ? <ChevronDown className="tl-reasoning-chev" /> : <ChevronRight className="tl-reasoning-chev" />}
        <Brain className="tl-reasoning-icon" aria-hidden="true" />
        <span className="tl-reasoning-label">{label}</span>
        {!active && total > 1 && <span className="tl-reasoning-count">共 {total} 段</span>}
      </button>
      {expanded && (
        <div className="tl-reasoning-row">
          <MarkdownBody content={clean} onPathClick={() => {}} />
        </div>
      )}
    </div>
  );
}
