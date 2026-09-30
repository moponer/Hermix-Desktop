import type { ElementType, ReactNode } from "react";

import { cx } from "../../lib/cx";

export type EmptyStateSize = "sm" | "md" | "lg";
export type EmptyStateVariant = "dashed" | "plain" | "inline";

export interface EmptyStateProps {
  icon?: ReactNode;
  title: string;
  description?: ReactNode;
  action?: ReactNode;
  size?: EmptyStateSize;
  variant?: EmptyStateVariant;
  as?: ElementType;
  className?: string;
}

const SIZE: Record<EmptyStateSize, { box: string; icon: string; title: string; desc: string }> = {
  sm: { box: "py-6 px-4", icon: "size-7", title: "text-[length:var(--text-ui)]", desc: "text-[length:var(--text-meta)]" },
  md: { box: "py-10 px-6", icon: "size-10", title: "text-[length:var(--text-body)]", desc: "text-[length:var(--text-meta)]" },
  lg: { box: "py-16 px-8", icon: "size-14", title: "text-[length:var(--text-title)]", desc: "text-[length:var(--text-ui)]" },
};

/**
 * 空态。`dashed` 是「这里应该有东西但还没有」；`plain` 用于纸面阅读区
 * （不要虚线框把正文切成一块块）；`inline` 只占一行，跟在正文后面。
 */
export function EmptyState({
  icon,
  title,
  description,
  action,
  size = "md",
  variant = "dashed",
  as: Tag = "div",
  className,
}: EmptyStateProps) {
  const s = SIZE[size];
  return (
    <Tag
      className={cx(
        "flex flex-col items-center justify-center text-center",
        variant !== "inline" && s.box,
        variant === "dashed" &&
          "rounded-[var(--radius-lg)] border border-dashed border-[var(--color-border-secondary)]",
        className,
      )}
    >
      {icon && (
        <div
          className={cx(
            "mb-3 grid place-items-center rounded-[var(--radius-lg)] text-[var(--color-text-tertiary)]",
            "bg-[var(--color-bg-tertiary)]",
            s.icon,
          )}
        >
          {icon}
        </div>
      )}
      <div className={cx("font-medium text-[var(--color-text-primary)]", s.title)}>{title}</div>
      {description && (
        <p className={cx("mt-1 max-w-sm text-[var(--color-text-tertiary)]", s.desc)}>{description}</p>
      )}
      {action && <div className="mt-4">{action}</div>}
    </Tag>
  );
}
