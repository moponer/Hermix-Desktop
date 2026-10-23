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
export {
  SettingsSection as Section,
  SettingsGrid as Grid,
  SettingsStatBar as SummaryBar,
} from "../settings/SettingsSection";

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
