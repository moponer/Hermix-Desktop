import { memo } from "react";
import "../styles/inline-thinking-dot.css";

/**
 * InlineThinkingDot — "打字机光标 + 段间隙指示器" 复合组件
 *
 * 主人需求 (2026-06-12 13:22)：
 *  - 每段输出结束后，下一段还没开始时 → 在段下方行首显示动态指示器
 *  - 下一段第一个 chunk 到达 → 立即替换为真实文字
 *  - 段内持续输出时（不显示在段内，要跟真实文字不重叠）
 *  - 整条回复结束 → 指示器消失
 *
 * 设计：组件本身只负责渲染一个"心跳式"圆点 + 心电图扫描线 + 可选 hint
 * 文字。挂载位置由父组件（BodyMarkdown / StreamingBubble）决定：
 *
 *  - **段内光标模式**（default 行为）：父组件把本组件挂在段末尾，作为
 *    打字机光标，跟 markdown 文本并排显示。流式 chunk 持续到达时
 *    父组件的文本变长，本组件的 AnimatedDots 一直停在段末"游标"位。
 *
 *  - **段间隙模式**（`asGap` 模式）：父组件用本组件替换段边界位置——
 *    500ms 内无新 chunk 时显示，挂在上一段的最末行下方行首，新 chunk
 *    到达时父组件从 markdown 树里移除本组件并插入新段。
 *
 * 动画全部走 CSS keyframes（h-think-pulse / h-think-sweep），无 JS 计时，
 * 不会阻塞流式渲染。
 *
 * **使用约束**：本组件设计成"无副作用、可独立渲染"。具体挂载到
 *  `<BodyMarkdown>` / `<StreamingBubble>` 的代码由其他开发者在他们的
 *  commit 中集成。
 */
interface InlineThinkingDotProps {
  /** 当作"段间隙指示器"渲染：display: block、独占一行、行首对齐 */
  asGap?: boolean;
  /** 自定义提示文字（默认 "正在思考…"） */
  hint?: string;
  /** 安静模式：心率点变灰、扫描线透明度降低，用于"等最后批次" */
  quiet?: boolean;
  /** 包裹元素的自定义 className */
  className?: string;
  /** ARIA：用于辅助技术朗读 */
  ariaLabel?: string;
}

function InlineThinkingDotImpl({
  asGap = false,
  hint = "正在思考…",
  quiet = false,
  className,
  ariaLabel,
}: InlineThinkingDotProps) {
  const classes = [
    "inline-thinking-dot",
    asGap ? "inline-thinking-dot--gap" : "inline-thinking-dot--cursor",
    quiet ? "inline-thinking-dot--quiet" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <span
      className={classes}
      role="status"
      aria-live="polite"
      aria-label={ariaLabel ?? hint}
      data-thinking-mode={asGap ? "gap" : "cursor"}
    >
      <span className="inline-thinking-dot__pulse" aria-hidden="true" />
      <svg
        viewBox="0 0 60 12"
        className="inline-thinking-dot__sweep"
        preserveAspectRatio="none"
        aria-hidden="true"
      >
        <path
          d="M 0 6 L 6 6 L 9 3 L 12 9 L 15 1 L 18 11 L 21 6 L 30 6 L 33 3 L 36 9 L 39 1 L 42 11 L 45 6 L 60 6"
          fill="none"
          strokeWidth="1.2"
          strokeLinecap="round"
          strokeLinejoin="round"
        />
      </svg>
      {/* v3 2026-06-18 主人反馈: gap 模式的文字 hint (如 "⚠ 响应较慢")
          跟父组件 StreamingBubble 的行头状态徽章 ("正在生成... 响应较慢")
          重复, 与 "terminal 运行中" 子行是同一种反模式 — 同一信息
          讲两次. 段间隙模式的视觉 (pulse + sweep) 已经够说明
          "上一段结束, 下一段还没来", 文字交给父徽章就行. cursor
          模式不受影响, 那里没有父徽章跟它竞争. */}
      {hint && !asGap && <span className="inline-thinking-dot__hint">{hint}</span>}
    </span>
  );
}

export const InlineThinkingDot = memo(InlineThinkingDotImpl);
