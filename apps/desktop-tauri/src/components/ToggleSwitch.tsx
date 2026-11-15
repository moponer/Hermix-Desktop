/**
 * 通用开关（2026-10-23 重做）。
 *
 * ## 为什么重做
 *
 * 旧版是 24×44px（h-6 w-11）、滑钮 20px、滑钮里还塞了 ✓ / ✕ 图标。
 * 三个问题叠在一起，导致它在列表右侧极其突兀：
 *   1. **尺寸过大**：44px 宽的开关出现在 32px 高的行里，是整行最重的元素，
 *      视线每次都被它拽走 —— 开关是次要操作，不该比标题更抢眼；
 *   2. **滑钮里放图标是反模式**：滑块本身就是位置信号（左=关、右=开），
 *      再叠一个勾/叉等于同一件事说三遍，而且在 20px 的圆里放 14px 图标，
 *      笔画挤在一起糊成一团。iOS / Material / macOS 的开关都不放图标；
 *   3. **宽度不跟随文字对齐**：44px 让右侧"动作区"宽度跳动，多个卡片并排
 *      时开关左边缘参差不齐。
 *
 * 现在是 36×20px（h-5 w-9）、滑钮 16px、无图标、位移动画走 transform。
 * 36px 恰好等于三档控件最小宽度，卡片右缘能对齐成一条线。
 *
 * ## 配色归入统一体系
 *
 * 开启态用 `--color-accent`（全站唯一的主行动色），不再用 success 绿 ——
 * 语义绿留给"状态=正常"的文案与图标，开关表达的是"我把它打开了"这个
 * 用户动作，属于主行动。这样全站的"可点可改"元素只有一种主色，
 * 换主题时只需改 accent 一处。
 */
import { type MouseEventHandler } from "react";
import { cx } from "../lib/cx";

interface ToggleSwitchProps {
  enabled: boolean;
  onToggle: () => void;
  disabled?: boolean;
  /** aria-label 在 enabled 时的描述（点它就是关闭） */
  offLabel?: string;
  /** aria-label 在 enabled=false 时的描述（点它就是启用） */
  onLabel?: string;
  className?: string;
}

export function ToggleSwitch({
  enabled,
  onToggle,
  disabled,
  offLabel = "关闭",
  onLabel = "启用",
  className,
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
      className={cx(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full",
        "transition-colors duration-[var(--duration-fast)] ease-[var(--ease-out)]",
        /* 焦点走全站统一的扁平淡晕（base.css 的 :focus-visible），
           这里不画 ring —— ring 是一圈描边。 */
        "focus-visible:outline-none",
        enabled ? "bg-accent" : "bg-bg-active",
        disabled ? "cursor-not-allowed opacity-40" : "cursor-pointer",
        className,
      )}
    >
      <span
        aria-hidden
        className={cx(
          /* 16px 白钮 + 一层极淡投影：投影只用来把白钮从底色上"抬"起来半像素，
             不用大范围阴影（那会让开关看起来像浮在页面之上）。 */
          "pointer-events-none absolute top-0.5 h-4 w-4 rounded-full bg-white",
          "shadow-[0_1px_2px_color-mix(in_srgb,var(--color-text-primary)_28%,transparent)]",
          "transition-transform duration-[var(--duration-fast)] ease-[var(--ease-out)]",
          enabled ? "translate-x-[18px]" : "translate-x-0.5",
        )}
      />
    </button>
  );
}
