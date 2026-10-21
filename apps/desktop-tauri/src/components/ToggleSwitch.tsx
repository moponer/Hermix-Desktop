/**
 * 通用开关组件 — 启用=绿色(success), 禁用=灰色(bg-tertiary)
 *
 * 语义：
 *   - enabled: 绿色背景 + 白色滑钮 + ✓ 图标
 *   - disabled: 灰色背景 + 灰色滑钮 + ✕ 图标
 *
 * 2026-10-21（视觉细节批次）：
 *   - 勾/叉改为直接用 lucide-react 的 Check / X。此前是手写内联 SVG 副本，
 *     全 app 同一个「对勾」因此有 5 种笔画粗细（1.6 / 1.8 / 2 / 2.2 / 2.5）。
 *     现在统一交给 Lucide 默认（strokeWidth = 2），不写显式 strokeWidth。
 *   - 关闭态不再靠描边表达「关」：去掉 border-border-secondary，改纯底色差
 *     （bg-bg-tertiary，与 ui/Switch 同款）—— 用户偏好：UI 一律不用描边。
 *   - 焦点环从 success 绿改成 accent（与 ui/Switch / ui/Button 一致）：
 *     焦点环是「焦点在哪」的信号，不是「状态对不对」的信号。
 */
import { type MouseEventHandler } from "react";
import { Check, X } from "lucide-react";

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
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent focus-visible:ring-offset-2 focus-visible:ring-offset-transparent",
        enabled ? "bg-success" : "bg-bg-tertiary",
        disabled ? "opacity-50 cursor-not-allowed" : "cursor-pointer",
        className,
      ].join(" ")}
    >
      <span
        className={[
          "pointer-events-none absolute top-[3px] flex items-center justify-center rounded-full shadow transition-colors duration-base",
          enabled ? "left-[23px] w-5 h-5 bg-white text-success" : "left-[3px] w-5 h-5 bg-bg-card text-text-muted",
        ].join(" ")}
      >
        {enabled
          ? <Check className="icon-sm" aria-hidden="true" />
          : <X className="icon-sm" aria-hidden="true" />}
      </span>
    </button>
  );
}
