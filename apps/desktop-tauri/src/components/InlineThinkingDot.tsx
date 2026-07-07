import { memo } from "react";
import "../styles/inline-thinking-dot.css";

/**
 * InlineThinkingDot — 极简 "模型思考中" 指示器
 *
 * 主人反馈 2026-07-06: LLM 思考状态视觉重复（"正在生成… 正在思考…" + 底部
 * 重复文字）。本组件改为单一、克制的指示器：
 *  - 默认：一颗呼吸光点 + 可选文字 hint
 *  - 慢响应：琥珀色光点 + 较慢呼吸
 *  - 段间隙/段内游标模式：只保留光点，不重复文字
 *  - 移除心电图扫描线，避免信息噪音
 *
 * 使用约束：父组件控制是否显示文字，本组件只负责视觉节奏。
 */
interface InlineThinkingDotProps {
  /** 当作"段间隙指示器"渲染：独占一行，用于段落边界 */
  asGap?: boolean;
  /** 自定义提示文字（默认空；父组件决定是否需要文字） */
  hint?: string;
  /** 安静模式：心率减慢、透明度降低 */
  quiet?: boolean;
  /** 视觉变体：default 蓝紫呼吸，warning 琥珀慢呼吸 */
  variant?: "default" | "warning";
  /** 尺寸：sm 用于 meta 行，md 用于占位区 */
  size?: "sm" | "md";
  /** 包裹元素的自定义 className */
  className?: string;
  /** ARIA：用于辅助技术朗读 */
  ariaLabel?: string;
  /** 是否完全隐藏文字（即使 hint 有值） */
  hideHint?: boolean;
}

function InlineThinkingDotImpl({
  asGap = false,
  hint,
  quiet = false,
  variant = "default",
  size = "md",
  className,
  ariaLabel,
  hideHint = false,
}: InlineThinkingDotProps) {
  const classes = [
    "inline-thinking-dot",
    asGap ? "inline-thinking-dot--gap" : "inline-thinking-dot--cursor",
    quiet ? "inline-thinking-dot--quiet" : "",
    variant === "warning" ? "inline-thinking-dot--warning" : "",
    size === "sm" ? "inline-thinking-dot--sm" : "",
    className ?? "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <span
      className={classes}
      role="status"
      aria-live="polite"
      aria-label={ariaLabel ?? hint ?? "思考中"}
      data-thinking-mode={asGap ? "gap" : "cursor"}
    >
      <span className="inline-thinking-dot__orb" aria-hidden="true">
        <span className="inline-thinking-dot__orb-core" />
        <span className="inline-thinking-dot__orb-glow" aria-hidden="true" />
      </span>
      {hint && !hideHint && (
        <span className="inline-thinking-dot__hint">{hint}</span>
      )}
    </span>
  );
}

export const InlineThinkingDot = memo(InlineThinkingDotImpl);
