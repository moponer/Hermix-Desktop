import { cx } from "../../lib/cx";

export type SwitchSize = "sm" | "md";

export interface SwitchProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  disabled?: boolean;
  /** 无可见标签时的无障碍名。 */
  label?: string;
  size?: SwitchSize;
  className?: string;
}

/* 尺寸写死成 token-free 的固定像素：开关是"能不能点"的可供性控件，
   尺寸随字号缩放会让一列开关参差不齐，反而更难扫读。 */
const TRACK = {
  sm: "h-4 w-7",
  md: "h-5 w-9",
} as const;
const KNOB = {
  sm: "h-3 w-3",
  md: "h-4 w-4",
} as const;
/* 轨道内边距 2px；开态位移 = 轨道宽 − 滑块宽 − 2×2。 */
const TRAVEL = {
  sm: "translate-x-3.5",
  md: "translate-x-4.5",
} as const;

/**
 * 设置页专用开关：36×20 胶囊 + 滑块，跟随 `--color-accent`。
 *
 * 与组件根目录那份 `ToggleSwitch.tsx` 的区别不是"新旧"，而是语义：
 * 老的写死 success 绿、轨道 44px、带 ✓/✕ 图标，服务于"启用某能力"的
 * 强提示场景；本组件走强调色、体积更小，是设置行右端的默认控件。
 * 两者都要留着——直接把老的改色会牵动已上线的调用点。
 */
export function Switch({
  checked,
  onChange,
  disabled,
  label = "切换设置",
  size = "md",
  className,
}: SwitchProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cx(
        "relative shrink-0 rounded-full transition-colors duration-150",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40",
        "disabled:cursor-not-allowed disabled:opacity-50",
        TRACK[size],
        checked ? "bg-[var(--color-accent)]" : "bg-[var(--color-bg-tertiary)]",
        className,
      )}
    >
      <span
        aria-hidden
        className={cx(
          "pointer-events-none absolute top-0.5 left-0.5 rounded-full bg-white shadow-[var(--shadow-card)]",
          "transition-transform duration-150",
          KNOB[size],
          checked && TRAVEL[size],
        )}
      />
    </button>
  );
}
