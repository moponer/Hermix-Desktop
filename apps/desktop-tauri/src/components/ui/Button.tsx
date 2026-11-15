import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from "react";

import { cx } from "../../lib/cx";
import { Spinner } from "./Spinner";

export type ButtonVariant =
  | "primary"
  /** 品牌绿（松墨绿）实底。只给一个面上「不容错过」的那一个动作 —— 输入框的发送键。 */
  | "accent"
  | "secondary"
  | "tonal"
  /** 品牌绿描边，hover 反转为实底。次要动作。 */
  | "tonal-outline"
  | "ghost"
  | "danger"
  | "danger-outline"
  | "link"
  | "inverse";

/** `base` 夹在 `sm` 与 `md` 之间（h-8），字母序上不合群，但那是现存按钮
 *  用得最多的高度。 */
export type ButtonSize = "xs" | "sm" | "base" | "md" | "lg";

/** 圆形：只放图标，正方、满圆。必须配 `aria-label`。 */
export type ButtonShape = "default" | "circle";

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  size?: ButtonSize;
  shape?: ButtonShape;
  /** 用 Spinner 替换前置图标并禁用按钮，同时置 `aria-busy`。 */
  loading?: boolean;
  icon?: ReactNode;
  iconPosition?: "start" | "end";
  block?: boolean;
}

/**
 * 禁用态样式按变体分开写。
 *
 * 一条 `disabled:opacity-50` 表达不了 primary 的禁用态 —— 半透明墨块压在
 * 页面上读起来像「还在加载」而不是「不可用」。
 */
const DISABLED_FILL =
  "disabled:bg-[var(--color-bg-hover)] disabled:text-[var(--color-text-tertiary)] disabled:shadow-none disabled:border-transparent";
const DISABLED_FADE = "disabled:opacity-50";

const VARIANT_CLASSES: Record<ButtonVariant, string> = {
  // 实心墨块、hover 压深一档。刻意不用品牌色打底：在一屏品牌绿已经到处
  // 标记强调的界面里，墨块才读得出「这是个按钮」。
  // hover 此前指向 --color-accent（亮色下与本体同值 = hover 无效），改指向
  // 主题各自的 hover 档。
  primary: `bg-[var(--color-btn-primary-bg)] text-[var(--color-btn-primary-text)] shadow-[var(--shadow-button-primary)] hover:bg-[var(--color-btn-primary-hover)] hover:-translate-y-px active:translate-y-0 active:scale-[0.97] ${DISABLED_FILL}`,
  accent: `bg-[var(--color-accent)] text-[var(--color-on-primary)] shadow-[var(--shadow-button-primary)] hover:bg-[var(--color-accent-hover)] hover:-translate-y-px active:translate-y-0 active:scale-[0.97] ${DISABLED_FILL}`,
  secondary: `bg-[var(--color-bg-card)] text-[var(--color-text-primary)] hover:bg-[var(--color-bg-hover)] active:scale-[0.98] ${DISABLED_FILL}`,
  tonal: `bg-[var(--color-accent-soft)] text-[var(--color-accent)] hover:bg-[var(--color-accent-soft-strong)] active:scale-[0.98] ${DISABLED_FILL}`,
  "tonal-outline": `bg-transparent text-[var(--color-accent)] border border-[var(--color-primary-fixed-dim)] hover:bg-[var(--color-accent)] hover:border-[var(--color-accent)] hover:text-[var(--color-on-primary)] active:scale-[0.98] ${DISABLED_FADE}`,
  ghost: `bg-transparent text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-hover)] hover:text-[var(--color-text-primary)] active:scale-[0.98] ${DISABLED_FADE}`,
  danger: `bg-[var(--color-error)] text-[var(--color-on-error)] hover:bg-[color-mix(in_srgb,var(--color-error)_88%,black)] hover:-translate-y-px active:translate-y-0 active:scale-[0.97] ${DISABLED_FILL}`,
  "danger-outline": `bg-transparent text-[var(--color-error)] border border-[var(--color-error)] hover:bg-[var(--color-error-soft)] active:scale-[0.98] ${DISABLED_FADE}`,
  link: `bg-transparent text-[var(--color-accent)] underline-offset-2 hover:underline hover:text-[var(--color-accent-hover)] px-0 ${DISABLED_FADE}`,
  inverse: `bg-[var(--color-inverse-surface)] text-[var(--color-inverse-on-surface)] hover:bg-[var(--color-accent)] hover:-translate-y-px active:translate-y-0 active:scale-[0.97] ${DISABLED_FILL}`,
};

/** 每档钉死一个高度。之前只设内边距，直接导致全应用 18 种按钮高度 ——
 *  任何想要特定高度的调用方都只能绕过组件自己写。 */
const SIZE_CLASSES: Record<ButtonSize, string> = {
  xs: "h-5 text-[length:var(--text-meta)] gap-1",
  sm: "h-6 text-xs gap-1.5",
  base: "h-8 text-xs gap-1.5",
  md: "h-9 text-sm gap-1.5",
  lg: "h-10 text-sm gap-2",
};

/** 水平内边距与尺寸分开，`link` 才能丢掉它。否则 link 会同时吐出
 *  `px-0` 和尺寸档的 `px-4`，赢的是谁由样式表排序决定。 */
const SIZE_PX: Record<ButtonSize, string> = { xs: "px-1.5", sm: "px-2", base: "px-3", md: "px-4", lg: "px-5" };

/** 圆形按钮的宽，必须等于同档高度，否则就不是圆。 */
const SIZE_SQUARE: Record<ButtonSize, string> = { xs: "w-5", sm: "w-6", base: "w-8", md: "w-9", lg: "w-10" };

const SPINNER_SIZE: Record<ButtonSize, number> = { xs: 11, sm: 12, base: 14, md: 16, lg: 16 };

/** 圆角也单独放，原因同上：绝不能同时吐出两个 radius。 */
const SHAPE_RADIUS: Record<ButtonShape, string> = {
  default: "rounded-[var(--radius-md)]",
  circle: "rounded-full",
};

const BASE_CLASSES = [
  "inline-flex items-center justify-center",
  "font-medium cursor-pointer",
  // 一条动效曲线走到底：着色 .16s，形变 .14s。只写 transition-colors 会让
  // hover 的抬升动作「弹」一下。
  "transition-[background-color,color,border-color,box-shadow,transform,opacity] duration-150 ease-out",
  /* 偏移用透明色：缝隙透出按钮所处的表面色。此前写死 --color-bg-card，
     按钮坐在 --color-bg-primary（白）上时会出现一道浅灰接缝。 */
  "disabled:cursor-not-allowed disabled:pointer-events-none disabled:translate-y-0",
  "motion-reduce:transition-none motion-reduce:hover:translate-y-0 motion-reduce:active:scale-100",
].join(" ");

/**
 * 有可见文字的按钮。纯图标控件用 `IconButton`（它强制要求无障碍名）。
 *
 * `type` 默认 `"button"`：原生默认是 `"submit"`，任何被丢进 `<form>` 的按钮
 * 都会在点击时提交表单 —— 只在有人给外面那圈包上 form 之后才暴露的 bug。
 */
export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  {
    variant = "primary",
    size = "md",
    shape = "default",
    loading = false,
    icon,
    iconPosition = "start",
    block = false,
    type = "button",
    disabled,
    children,
    className,
    ...props
  },
  ref,
) {
  const leading = loading ? <Spinner size={SPINNER_SIZE[size]} /> : iconPosition === "start" ? icon : null;
  const trailing = !loading && iconPosition === "end" ? icon : null;

  return (
    <button
      ref={ref}
      type={type}
      disabled={disabled || loading}
      aria-busy={loading || undefined}
      className={cx(
        BASE_CLASSES,
        SHAPE_RADIUS[shape],
        VARIANT_CLASSES[variant],
        SIZE_CLASSES[size],
        shape === "circle" ? SIZE_SQUARE[size] : variant !== "link" && SIZE_PX[size],
        block && "w-full",
        className,
      )}
      {...props}
    >
      {leading}
      {children}
      {trailing}
    </button>
  );
});

export interface IconButtonProps extends Omit<ButtonProps, "children" | "icon"> {
  icon: ReactNode;
  /** 无障碍名。`shape="circle"` 时必填。 */
  label: string;
}

/** 纯图标按钮。`label` 必填 —— 图标按钮没有可读文本，缺了它就只剩一坨像素。 */
export const IconButton = forwardRef<HTMLButtonElement, IconButtonProps>(function IconButton(
  { icon, label, shape = "default", className, ...props },
  ref,
) {
  return (
    <Button ref={ref} shape={shape} aria-label={label} title={label} className={className} {...props}>
      {icon}
    </Button>
  );
});
