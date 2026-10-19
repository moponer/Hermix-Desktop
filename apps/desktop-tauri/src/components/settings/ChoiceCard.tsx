import type { ReactNode } from "react";

import { cx } from "../../lib/cx";

/**
 * 设置页的选择卡（外观页明暗三选、通用页工作模式二选共用）。
 *
 * 为什么抽成共享件：两处卡片的需求完全一样（图标 + 标题 + 说明 + 选中态），
 * 各自写一份必然漂移——上次就是两边都"没有层次、图标没颜色"，
 * 因为它们都是随手写的默认排版。收敛到一处后改一次两处都生效。
 *
 * 层次由四层承担，从强到弱：
 *   1. 图标色块（唯一的彩色，视线第一落点）
 *   2. 标题（正文级字号 + primary 色）
 *   3. 说明（meta 字号 + muted 色，允许两行）
 *   4. 选中指示（强调色描边 + 底色微染 + 单选环）
 * 靠"一彩三灰"的对比建立主次，而不是给每层都上色。
 */
export type CardTone = "accent" | "success" | "warning" | "error" | "info";

/**
 * 每个色调给「柔和底 / 饱和图标」两支。**选中态不换色**——只换描边、
 * 标题色和单选环。
 *
 * 为什么不做"选中时底色填成实色 + 白色图标"：一是白图标要写死字面量
 * （会被 check-theme-tokens 的 bare-color 拦下，且它不属于任何语义槽）；
 * 二是实色块压在纸墨底上视觉太重，与本项目"侧栏几乎无彩色、强调色只
 * 留给主操作"的取向相反。柔和底 + 饱和图标在未选中时就有识别度，
 * 选中再叠描边与单选环，足够明确。
 */
const TONE: Record<CardTone, { chip: string; icon: string }> = {
  accent: { chip: "var(--color-accent-on-text)", icon: "var(--color-accent)" },
  success: { chip: "var(--color-success-soft)", icon: "var(--color-success)" },
  warning: { chip: "var(--color-warning-soft)", icon: "var(--color-warning)" },
  error: { chip: "var(--color-error-soft)", icon: "var(--color-error)" },
  info: { chip: "var(--color-info-soft)", icon: "var(--color-info)" },
};

export interface ChoiceCardProps {
  label: string;
  description: string;
  icon: ReactNode;
  tone: CardTone;
  selected: boolean;
  onSelect: () => void;
  /** 次要标注（如「跟随系统」当前解析成深色）。 */
  meta?: string;
  disabled?: boolean;
}

export function ChoiceCard({
  label, description, icon, tone, selected, onSelect, meta, disabled,
}: ChoiceCardProps) {
  const t = TONE[tone];
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      disabled={disabled}
      onClick={onSelect}
      className={cx(
        "flex w-full flex-col items-start gap-2.5 rounded-[var(--radius-lg)] p-4 text-left transition-colors",
        "shadow-[var(--shadow-paper)]",
        "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]/40",
        "disabled:cursor-not-allowed disabled:opacity-50",
        selected
          ? "bg-[var(--color-accent-soft)] ring-1 ring-[var(--color-accent)]"
          : "bg-[var(--color-bg-card)] hover:bg-[var(--color-bg-card-hover)]",
      )}
    >
      <div className="flex w-full items-center gap-2.5">
        <span
          aria-hidden
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-[var(--radius-md)]"
          style={{ background: t.chip, color: t.icon }}
        >
          {icon}
        </span>
        <span className="min-w-0 flex-1">
          <span
            className="block text-[length:var(--text-body)] font-medium leading-tight"
            style={{ color: selected ? "var(--color-accent)" : "var(--color-text-primary)" }}
          >
            {label}
          </span>
          {meta && (
            <span className="mt-0.5 block text-[length:var(--text-tiny)] leading-tight text-[var(--color-text-muted)]">
              {meta}
            </span>
          )}
        </span>
        {/* 单选环：色觉障碍下靠形状也能判选中，不只靠颜色 */}
        <span
          aria-hidden
          className="flex h-4 w-4 shrink-0 items-center justify-center rounded-full border-2 transition-colors"
          style={{
            borderColor: selected ? "var(--color-accent)" : "var(--color-border-primary)",
            background: selected ? "var(--color-accent)" : "transparent",
            color: "var(--color-on-accent)",
          }}
        >
          {selected && (
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
              <path
                d="M2 5l2 2 4-4"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
          )}
        </span>
      </div>

      <span className="text-[length:var(--text-ui)] leading-relaxed text-[var(--color-text-muted)]">
        {description}
      </span>
    </button>
  );
}
