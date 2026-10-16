import { cx } from "../../lib/cx";

/**
 * Skeleton —— 骨架屏原语（2026-10-16，补 12 缺失原语之一）。
 *
 * 加载态用"内容形状的灰色块 + 呼吸动画"占位，替代转圈/"加载中"文字：
 * 感知速度更快、加载完成零布局跳动（块的位置即内容的位置）。
 * 零色相纪律：底色走 bg-tertiary（中性），不进强调色。
 *
 * 用法：
 *   <Skeleton className="h-4 w-40" />            任意形状块
 *   <SkeletonText lines={3} />                    多行文字条（末行 60% 宽）
 *   <SkeletonCard lines={2} />                    卡片容器 + 内部文字条
 */
export function Skeleton({ className }: { className?: string }) {
  return (
    <div
      aria-hidden
      className={cx(
        "animate-pulse rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)]",
        className,
      )}
    />
  );
}

export function SkeletonText({ lines = 3, className }: { lines?: number; className?: string }) {
  return (
    <div className={cx("flex flex-col gap-2", className)} aria-hidden>
      {Array.from({ length: lines }).map((_, i) => (
        <Skeleton
          key={i}
          className={cx("h-3.5", i === lines - 1 ? "w-3/5" : "w-full")}
        />
      ))}
    </div>
  );
}

export function SkeletonCard({ lines = 2, className }: { lines?: number; className?: string }) {
  return (
    <div
      aria-hidden
      className={cx(
        "rounded-[var(--radius-lg)] border border-[var(--color-border-primary)] bg-[var(--color-bg-card)] p-4",
        className,
      )}
    >
      <Skeleton className="mb-3 h-4 w-1/3" />
      <SkeletonText lines={lines} />
    </div>
  );
}
