import type { ReactNode } from "react";

import { cx } from "../../lib/cx";

export type BadgeTone = "neutral" | "brand" | "success" | "warning" | "danger" | "info";
export type BadgeVariant = "soft" | "outline";
export type BadgeSize = "xs" | "sm" | "md";

export interface BadgeProps {
  children: ReactNode;
  tone?: BadgeTone;
  /** 没有实心档：实心徽标在暗色主题上对比度不可控。 */
  variant?: BadgeVariant;
  size?: BadgeSize;
  bordered?: boolean;
  pill?: boolean;
  icon?: ReactNode;
  mono?: boolean;
  className?: string;
}

const SOFT: Record<BadgeTone, string> = {
  neutral: "bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]",
  brand: "bg-[var(--color-accent-soft)] text-[var(--color-accent)]",
  success: "bg-[var(--color-success-soft)] text-[var(--color-success)]",
  warning: "bg-[var(--color-warning-soft)] text-[var(--color-warning)]",
  danger: "bg-[var(--color-error-soft)] text-[var(--color-error)]",
  info: "bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]",
};

const OUTLINE: Record<BadgeTone, string> = {
  neutral: "text-[var(--color-text-secondary)] border-[var(--color-border-secondary)]",
  brand: "text-[var(--color-accent)] border-[var(--color-primary-fixed-dim)]",
  success: "text-[var(--color-success)] border-[var(--color-success)]",
  warning: "text-[var(--color-warning)] border-[var(--color-warning)]",
  danger: "text-[var(--color-error)] border-[var(--color-error)]",
  info: "text-[var(--color-text-secondary)] border-[var(--color-border-secondary)]",
};

const SIZE: Record<BadgeSize, string> = {
  xs: "px-2 py-0.5 text-[length:var(--text-meta)]",
  sm: "px-2 py-0.5 text-[length:var(--text-meta)]",
  md: "px-2.5 py-1 text-[length:var(--text-meta)]",
};

/** 状态点。`pulse` 用于「正在跑」—— 它是唯一该动的东西。 */
export function StatusDot({
  tone = "neutral",
  size = "md",
  pulse = false,
  className,
}: {
  tone?: BadgeTone;
  size?: "sm" | "md" | "lg";
  pulse?: boolean;
  className?: string;
}) {
  const dot = {
    sm: "size-1.5",
    md: "size-2",
    lg: "size-2.5",
  }[size];
  const color = {
    neutral: "bg-[var(--color-text-tertiary)]",
    brand: "bg-[var(--color-accent)]",
    success: "bg-[var(--color-success)]",
    warning: "bg-[var(--color-warning)]",
    danger: "bg-[var(--color-error)]",
    info: "bg-[var(--color-text-tertiary)]",
  }[tone];
  return (
    <span
      aria-hidden
      className={cx("inline-block shrink-0 rounded-full", dot, color, pulse && "animate-pulse", className)}
    />
  );
}

export function Badge({
  children,
  tone = "neutral",
  variant = "soft",
  size = "xs",
  bordered = false,
  pill = true,
  icon,
  mono = false,
  className,
}: BadgeProps) {
  return (
    <span
      className={cx(
        "inline-flex shrink-0 items-center gap-1 font-medium whitespace-nowrap",
        SIZE[size],
        pill ? "rounded-full" : "rounded-[var(--radius-sm)]",
        variant === "soft" ? SOFT[tone] : "bg-transparent",
        variant === "outline" && OUTLINE[tone],
        bordered && variant === "soft" && "border border-transparent",
        mono && "font-mono",
        className,
      )}
    >
      {icon}
      {children}
    </span>
  );
}
