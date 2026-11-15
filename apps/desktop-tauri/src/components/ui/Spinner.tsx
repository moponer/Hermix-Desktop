import { cx } from "../../lib/cx";

export interface SpinnerProps {
  size?: number;
  className?: string;
}

/** 忙碌指示。转圈速度刻意压到 1s 一圈 —— 工具在跑时用户要盯着看，
 *  太快会显得焦躁。
 *
 *  修复（2026-10-31）：此前只有 `borderTopColor: transparent` 而没有任何
 *  border 宽度/颜色 —— Tailwind preflight 把 border-width 归零，于是这个
 *  "转圈" 渲染出来是一块**完全透明**的圆：按钮 loading 态、"记忆管理"
 *  加载大图，用户看到的都是空白。补上 `border-2 border-current`（颜色随
 *  文字色走令牌，浅/深主题自动正确）。 */
export function Spinner({ size = 16, className }: SpinnerProps) {
  return (
    <span
      role="status"
      aria-label="加载中"
      className={cx(
        "inline-block shrink-0 animate-spin rounded-full border-2 border-current",
        className,
      )}
      style={{
        width: size,
        height: size,
        borderTopColor: "transparent",
        opacity: 0.7,
      }}
    />
  );
}
