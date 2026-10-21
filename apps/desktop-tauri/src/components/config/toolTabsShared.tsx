// toolTabsShared.tsx —— ToolTabs 四个 Tab 的共享 UI 件（R2-4 拆分第二步）。
// 从 54KB God Object 抽出：SearchBar / SummaryBar / Section / Grid / EmptyHint / SkeletonGrid。
import type { ReactNode } from "react";
import { Search } from "lucide-react";
import type { LucideIcon } from "lucide-react";

export function SearchBar({
  query,
  onQuery,
  right,
}: {
  query: string;
  onQuery: (q: string) => void;
  right?: ReactNode;
}) {
  return (
    <div className="flex items-center gap-2 mb-5">
      <div className="relative flex-1">
        <Search className="icon-md absolute left-3 top-1/2 -translate-y-1/2 text-text-muted" />
        <input
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder="搜索名称、描述或类别"
          className="w-full h-9 bg-bg-tertiary border-0 rounded-lg pl-9 pr-3 text-[length:var(--text-ui)] text-text-primary placeholder-text-muted outline-none focus-visible:ring-2 focus-visible:ring-[var(--color-accent)]"
        />
      </div>
      {right}
    </div>
  );
}

/**
 * 「共 N 个 · M 启用」统计带。
 *
 * 原先是一个 `border + bg-bg-card` 的方块，视觉上跟输入框同形，读起来像
 * 一个禁用的输入框而不是一句统计。改成：数字大、标签小、右侧一条进度计量。
 * 无描边（靠 bg-bg-card 浮出）。
 */
export function SummaryBar({
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
    <div className="mb-3 flex items-center gap-5 px-4 py-3 rounded-[14px] bg-bg-card">
      <div className="flex flex-col shrink-0">
        <span className="text-xl font-bold leading-none tracking-tight text-text-primary">{total}</span>
        <span className="text-[length:var(--text-tiny)] text-text-muted mt-0.5">{totalLabel}</span>
      </div>
      <div className="flex flex-col shrink-0">
        <span
          className="text-xl font-bold leading-none tracking-tight"
          style={{ color: "var(--color-text-success)" }}
        >
          {enabled}
        </span>
        <span className="text-[length:var(--text-tiny)] text-text-muted mt-0.5">{enabledLabel}</span>
      </div>
      <div className="flex-1 min-w-20">
        <div className="flex justify-between text-[length:var(--text-tiny)] text-text-muted mb-1.5">
          <span>启用占比</span>
          <span className="tabular-nums">{pct}%</span>
        </div>
        <div className="h-1.5 rounded-full bg-bg-tertiary overflow-hidden">
          <div
            className="h-full rounded-full transition-[width] duration-300"
            style={{ width: `${pct}%`, background: "var(--color-btn-primary-bg)" }}
          />
        </div>
      </div>
    </div>
  );
}

export function Section({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: ReactNode;
}) {
  return (
    <section className="mb-6">
      <div className="flex items-baseline justify-between mb-3">
        <h2 className="text-base font-medium text-text-primary">{title}</h2>
        {hint && <span className="text-[length:var(--text-meta)] text-text-muted">{hint}</span>}
      </div>
      {children}
    </section>
  );
}

export function Grid({
  children,
  empty,
  emptyText,
}: {
  children: ReactNode;
  empty: boolean;
  emptyText?: string;
}) {
  if (empty) {
    return (
      <div className="grid grid-cols-1 md:grid-cols-2 gap-3">
        <div className="col-span-full text-center text-sm text-text-muted py-6">{emptyText}</div>
      </div>
    );
  }
  return <div className="grid grid-cols-1 md:grid-cols-2 gap-3 pb-2">{children}</div>;
}

export function EmptyHint({ icon: Icon, text }: { icon: LucideIcon; text: string }) {
  return (
    <div className="flex flex-col items-center gap-2 text-text-muted py-10">
      <Icon className="icon-lg" />
      <p className="text-sm">{text}</p>
    </div>
  );
}

export function SkeletonGrid({ count }: { count: number }) {
  return (
    <div className="grid grid-cols-1 md:grid-cols-2 gap-3 pb-2">
      {Array.from({ length: count }).map((_, i) => (
        <div
          key={i}
          className="h-[68px] rounded-xl bg-bg-card/40 animate-pulse"
        />
      ))}
    </div>
  );
}
