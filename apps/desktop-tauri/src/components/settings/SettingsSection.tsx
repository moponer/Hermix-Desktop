import type { ReactNode } from "react";
import { cx } from "../../lib/cx";

/**
 * 设置类页面的三件套容器：分区 / 网格 / 统计条（2026-10-23 收敛）。
 *
 * ## 为什么收敛
 *
 * 「设置」这一层此前每个页各写一遍 `<section>` + `<h2>` + `<div className="grid …">`，
 * 于是同一套信息在不同页长得不一样：
 *   · 标题字号有 text-base / text-[length:--text-body] / 20px 三种；
 *   · 网格间距有 gap-3 / gap-4 / gap-2.5 三种；
 *   · 统计条有的用大数字 + 进度条双装饰，有的只有一句「共 N 个」。
 * 这些差异单看每页都"还行"，但连着点三四个页就会觉得"不是一个软件"。
 *
 * 收敛后只有一个实现，改一次全站生效 —— 这也是「一键可换」的前提：
 * **样式必须只有一个出处**，否则换主题/换密度就是逐页改。
 *
 * ## 层次（三层，不要再加）
 *
 *   分区标题（13px/600 primary）
 *     └ 说明（11px faint，可选，跟标题同一行右侧）
 *   内容（网格或任意节点）
 *
 * 不再画分隔线、不再给分区加底 —— 靠间距分组，这是扁平化纪律的一部分。
 */

export function SettingsSection({
  title,
  hint,
  action,
  children,
  className,
}: {
  title: ReactNode;
  /** 标题右侧的一句说明（数量、来源、注意事项）。 */
  hint?: ReactNode;
  /** 标题右侧的操作按钮（刷新、新增）。给了 action 就不显示 hint。 */
  action?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cx("mb-7 settings-section", className)}>
      <div className="mb-2.5 flex items-center justify-between gap-3 settings-section__head">
        <h2 className="text-[length:var(--text-ui)] font-semibold text-[var(--color-text-primary)] settings-section__title">
          {title}
        </h2>
        <div className="flex shrink-0 items-center gap-2">
          {action}
          {!action && hint && (
            <span className="text-[length:var(--text-tiny)] text-[var(--color-text-faint)] settings-section__hint">
              {hint}
            </span>
          )}
        </div>
      </div>
      {children}
    </section>
  );
}

export function SettingsGrid({
  children,
  empty,
  emptyText,
  cols = 2,
}: {
  children: ReactNode;
  empty?: boolean;
  emptyText?: ReactNode;
  /** 单列（行式列表）/ 双列（卡片）。默认双列。 */
  cols?: 1 | 2;
}) {
  const grid = cx(
    "grid gap-2",
    cols === 2 ? "grid-cols-1 md:grid-cols-2" : "grid-cols-1",
  );
  if (empty) {
    return (
      <div className={grid}>
        <div className="col-span-full py-6 text-center text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
          {emptyText}
        </div>
      </div>
    );
  }
  return <div className={grid}>{children}</div>;
}

/**
 * 统计条：一句「共 N 个 · 已启用 M」+ 一条 2px 细进度。
 *
 * 旧版用两个 20px 粗体数字并排 + 6px 进度条 + 三段文字标签，
 * 在一页设置里比正文还重 —— 它是背景信息，不该是视觉焦点。
 * 现在压成一行 12px 文本 + 2px 进度，只在需要"扫一眼比例"时起作用。
 */
export function SettingsStatBar({
  total,
  enabled,
  totalLabel,
  enabledLabel,
}: {
  total: number;
  enabled: number;
  totalLabel: string;
  enabledLabel: string;
}) {
  const pct = total > 0 ? Math.round((enabled / total) * 100) : 0;
  return (
    <div className="mb-3 flex items-center gap-3">
      <p className="shrink-0 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
        共 <span className="font-medium tabular-nums text-[var(--color-text-primary)]">{total}</span> {totalLabel}
        <span className="mx-1.5 text-[var(--color-text-faint)]">·</span>
        {enabledLabel} <span className="font-medium tabular-nums text-[var(--color-text-primary)]">{enabled}</span>
      </p>
      <div className="h-0.5 min-w-16 flex-1 overflow-hidden rounded-full bg-[var(--color-bg-tertiary)]">
        <div
          className="h-full rounded-full bg-[var(--color-accent)] transition-[width] duration-300 ease-out"
          style={{ width: `${pct}%` }}
        />
      </div>
      <span className="w-8 shrink-0 text-right text-[length:var(--text-tiny)] tabular-nums text-[var(--color-text-faint)]">
        {pct}%
      </span>
    </div>
  );
}
