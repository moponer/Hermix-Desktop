import { useRef, type ReactNode } from "react";

import { cx } from "../../lib/cx";

export type SegmentedAppearance = "solid" | "raised" | "underline";
export type SegmentedSize = "sm" | "md";

export interface SegmentedOption<T extends string> {
  value: T;
  label: ReactNode;
  disabled?: boolean;
}

export interface SegmentedControlProps<T extends string> {
  options: SegmentedOption<T>[];
  value: T;
  onChange: (value: T) => void;
  label: string;
  as?: "radiogroup" | "tablist";
  appearance?: SegmentedAppearance;
  size?: SegmentedSize;
  /** `fill` 让每项等分容器宽度。 */
  layout?: "auto" | "fill";
  className?: string;
}

const SIZE: Record<SegmentedSize, string> = { sm: "h-7 text-[length:var(--text-meta)]", md: "h-8 text-[length:var(--text-meta)]" };

const APPEARANCE: Record<SegmentedAppearance, string> = {
  solid: "rounded-[var(--radius-md)]",
  raised: "rounded-[var(--radius-md)] shadow-[var(--shadow-card)]",
  underline: "rounded-none",
};

/** 方向键漫游的 tabindex。原生 select 在触屏上要重做一整套，
 *  但在纯桌面场景下 segmented control 更贴切。 */
export function SegmentedControl<T extends string>({
  options,
  value,
  onChange,
  label,
  as = "radiogroup",
  appearance = "solid",
  size = "md",
  layout = "auto",
  className,
}: SegmentedControlProps<T>) {
  const ref = useRef<HTMLDivElement>(null);
  const move = (delta: number) => {
    const i = options.findIndex((o) => o.value === value);
    const next = options[(i + delta + options.length) % options.length];
    if (next && !next.disabled) onChange(next.value);
  };
  return (
    <div
      ref={ref}
      role={as}
      aria-label={label}
      className={cx(
        /* max-w-full + overflow-x-auto：窄容器里**横向滚动**，而不是把每个页签
           压成竖排字（中文没有词内换行点 → 一压就是一列字）。宽容器无变化。 */
        "inline-flex max-w-full gap-0.5 overflow-x-auto bg-[var(--color-bg-tertiary)] p-0.5",
        APPEARANCE[appearance],
        layout === "fill" && "flex w-full",
        className,
      )}
      onKeyDown={(e) => {
        if (e.key === "ArrowRight" || e.key === "ArrowDown") {
          e.preventDefault();
          move(1);
        } else if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
          e.preventDefault();
          move(-1);
        }
      }}
    >
      {options.map((o) => {
        const active = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role={as === "tablist" ? "tab" : "radio"}
            aria-checked={as === "tablist" ? undefined : active}
            aria-selected={as === "tablist" ? active : undefined}
            tabIndex={active ? 0 : -1}
            disabled={o.disabled}
            onClick={() => onChange(o.value)}
            className={cx(
              "inline-flex shrink-0 items-center justify-center gap-1.5 whitespace-nowrap px-3 font-medium transition-colors duration-150",
              "disabled:cursor-not-allowed disabled:opacity-50",
              layout === "fill" && "flex-1",
              SIZE[size],
              active
                ? cx(
                    "bg-[var(--color-bg-elevated)] text-[var(--color-text-primary)] shadow-[var(--shadow-card)]",
                    appearance === "underline" && "rounded-none shadow-none",
                  )
                : "text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)]",
            )}
          >
            {o.label}
          </button>
        );
      })}
    </div>
  );
}
