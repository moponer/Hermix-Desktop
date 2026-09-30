import { cx } from "../../lib/cx";

export interface SpinnerProps {
  size?: number;
  className?: string;
}

/** 忙碌指示。转圈速度刻意压到 1s 一圈 —— 工具在跑时用户要盯着看，
 *  太快会显得焦躁。 */
export function Spinner({ size = 16, className }: SpinnerProps) {
  return (
    <span
      role="status"
      aria-label="加载中"
      className={cx("inline-block shrink-0 animate-spin rounded-full", className)}
      style={{
        width: size,
        height: size,
        border: "2px solid currentColor",
        borderTopColor: "transparent",
        opacity: 0.7,
      }}
    />
  );
}
