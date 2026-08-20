/**
 * 通用开关组件 — 启用=绿色(success), 禁用=灰色(bg-tertiary)
 *
 * 语义：
 *   - enabled: 绿色背景 + 白色滑钮 + ✓ 图标
 *   - disabled: 灰色背景 + 灰色滑钮 + ✕ 图标
 *
 * 不依赖 lucide, 纯 SVG 内联, 避免 icon 包依赖漂移。
 */
import { type MouseEventHandler } from "react";

interface ToggleSwitchProps {
  enabled: boolean;
  onToggle: () => void;
  disabled?: boolean;
  /** aria-label 在 enabled 时的描述 (默认 "关闭") */
  offLabel?: string;
  /** aria-label 在 disabled 时的描述 (默认 "启用") */
  onLabel?: string;
  className?: string;
}

export function ToggleSwitch({
  enabled,
  onToggle,
  disabled,
  offLabel = "关闭",
  onLabel = "启用",
  className = "",
}: ToggleSwitchProps) {
  const handleClick: MouseEventHandler<HTMLButtonElement> = (e) => {
    if (disabled) return;
    e.preventDefault();
    e.stopPropagation();
    onToggle();
  };

  return (
    <button
      type="button"
      role="switch"
      aria-checked={enabled}
      aria-label={enabled ? offLabel : onLabel}
      disabled={disabled}
      onClick={handleClick}
      className={[
        "relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-success/40",
        enabled
          ? "bg-success"
          : "bg-bg-tertiary border border-border-secondary",
        disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer",
        className,
      ].join(" ")}
    >
      <span
        className={[
          "pointer-events-none absolute top-[3px] flex items-center justify-center rounded-full shadow transition-all duration-150",
          enabled ? "left-[23px] w-5 h-5 bg-white text-success" : "left-[3px] w-5 h-5 bg-bg-card text-text-muted",
        ].join(" ")}
      >
        {enabled
          ? <svg width="10" height="10" viewBox="0 0 10 10" fill="none"><path d="M2 5l2 2 4-4" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round"/></svg>
          : <svg width="10" height="10" viewBox="0 0 10 10" fill="none"><path d="M2.5 2.5l5 5M7.5 2.5l-5 5" stroke="currentColor" strokeWidth="1.6" strokeLinecap="round"/></svg>
        }
      </span>
    </button>
  );
}
