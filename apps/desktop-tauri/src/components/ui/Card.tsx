import type { ElementType, ReactNode } from "react";

import { cx } from "../../lib/cx";

export type CardRadius = "sm" | "md" | "lg" | "xl" | "2xl";
export type CardSurface = "base" | "low" | "lowest" | "high" | "container" | "none";
export type CardBorder = "solid" | "dashed" | "none";
export type CardPadding = "none" | "sm" | "md" | "lg";

export interface CardProps {
  children: ReactNode;
  radius?: CardRadius;
  surface?: CardSurface;
  border?: CardBorder;
  padding?: CardPadding;
  shadow?: "none" | "card" | "composer";
  /** 整卡可点：悬停抬升 + 光标变化。 */
  interactive?: boolean;
  /** 抬升强度，仅 interactive 时有意义。 */
  lift?: "sm" | "md";
  as?: ElementType;
  className?: string;
  onClick?: () => void;
  style?: React.CSSProperties;
}

const RADIUS: Record<CardRadius, string> = {
  sm: "rounded-[var(--radius-sm)]",
  md: "rounded-[var(--radius-md)]",
  lg: "rounded-[var(--radius-lg)]",
  xl: "rounded-[var(--radius-xl)]",
  "2xl": "rounded-[var(--radius-2xl)]",
};

/**
 * 六个面档，对应 cc-haha 的 `--color-surface*`。`none` 是给「纸面」用的：
 * 阅读区不该有面，靠排版分层。
 */
const SURFACE: Record<CardSurface, string> = {
  base: "bg-[var(--color-bg-card)]",
  low: "bg-[var(--color-bg-secondary)]",
  lowest: "bg-[var(--color-bg-primary)]",
  high: "bg-[var(--color-bg-hover)]",
  container: "bg-[var(--color-bg-tertiary)]",
  none: "bg-transparent",
};

const BORDER: Record<CardBorder, string> = {
  solid: "border border-[var(--color-border-primary)]",
  dashed: "border border-dashed border-[var(--color-border-secondary)]",
  none: "border-0",
};

const PADDING: Record<CardPadding, string> = {
  none: "p-0",
  sm: "p-2.5",
  md: "p-4",
  lg: "p-5",
};

const SHADOW = {
  none: "",
  card: "shadow-[var(--shadow-card)]",
  composer: "shadow-[var(--shadow-pop)]",
} as const;

/**
 * 通用容器。
 *
 * `border="none"` + `surface="none"` 是「纸面」组合 —— 阅读区靠字号、行距、
 * 段距分层，不靠边框和底色。满屏盒子就是这么来的：层次读不出来时，
 * 唯一的手段就是画线。
 */
export function Card({
  children,
  radius = "lg",
  surface = "base",
  border = "solid",
  padding = "md",
  shadow = "none",
  interactive = false,
  lift = "sm",
  as: Tag = "div",
  className,
  onClick,
  style,
}: CardProps) {
  return (
    <Tag
      onClick={onClick}
      style={style}
      className={cx(
        RADIUS[radius],
        SURFACE[surface],
        BORDER[border],
        PADDING[padding],
        SHADOW[shadow],
        interactive &&
          "cursor-pointer transition-[transform,box-shadow,background-color] duration-150 ease-out hover:-translate-y-px",
        interactive && lift === "md" && "hover:shadow-[var(--shadow-pop)]",
        interactive && lift === "sm" && "hover:shadow-[var(--shadow-card)]",
        className,
      )}
    >
      {children}
    </Tag>
  );
}
