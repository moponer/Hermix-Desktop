// Thinking/processing indicators previously colocated in MessageBubble.tsx.
import { Brain } from "lucide-react";
import { MarkdownBody } from "./MarkdownBody";

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

/**
 * 思考正文（L3 推理层）。
 *
 * 「这是旁注」的标记靠三个几何量，不靠色差 —— 深色下正文 primary 是
 * 15.32:1、思考 tertiary 是 4.77:1，都过 AA，色阶不缺；而能承载语义
 * 的那根竖线旧值只有 1.37:1，比它标记的正文还弱。真正扛得住的是：
 *
 *   1. 12px 缩进（`components-helpers.css` 的 `padding-left`）
 *   2. 这行**文字标签** —— 对比度无关，永远读得到
 *   3. 轨道提到 `--color-text-faint`（2.98:1，是旧值的两倍）
 *
 * 标签还有第二个作用：一屏里思考被折叠收起时，它告诉用户「这里有
 * 东西但你没看」，而不是让思考无声消失。
 */
export function ReasoningBlock({
  content,
  total = 1,
}: {
  content: string;
  /** 本轮共几段思考 —— >1 时标签补一个段数。 */
  total?: number;
}) {
  // Strip both <think> and <reasoning> wrappers — providers use both tags
  // for internal monologue, and the surrounding ProcessTimeline already
  // signals that this is process content.
  const clean = content
    .replace(/<\/?think>/gi, "")
    .replace(/<\/?reasoning>/gi, "")
    .trim();
  if (!clean) return null;

  return (
    <div className="tl-reasoning-wrap">
      <div className="tl-reasoning-label">
        <Brain />
        <span>思考过程</span>
        {total > 1 && <span>· 共 {total} 段</span>}
      </div>
      <div className="tl-reasoning-row">
        <MarkdownBody content={clean} onPathClick={() => {}} />
      </div>
    </div>
  );
}
