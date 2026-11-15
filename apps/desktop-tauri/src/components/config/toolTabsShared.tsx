// toolTabsShared.tsx —— ToolTabs 四个 Tab 的共享 UI 件（R2-4 拆分第二步）。
// 从 54KB God Object 抽出：SearchBar / SummaryBar / Section / Grid / EmptyHint / SkeletonGrid。
import type { ReactNode } from "react";
import { Search } from "lucide-react";
import type { LucideIcon } from "lucide-react";

/**
 * 工具页工具栏：搜索框（占满左侧）+ 可选右侧动作。
 *
 * **不设外边距**（2026-10-28）：四个工具页曾经各自在外面再套 mb-3/mb-4，
 * 与本组件自带的 mb-5 叠加成双间距，页面越往下越松。现在统一由父级的
 * `space-y-*` 控制节奏，工具栏自身只负责一行内的对齐。
 */
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
    <div className="flex w-full items-center gap-2">
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
 * 工具页的组标题 —— 统一四页的「区块名 + 计数」。
 *
 * 层级（自高到低，只有这五层）：
 *   页头 16/600 · 组标题 13/600 · 卡片标题 13/500 · 描述 12 · 元信息 12 faint
 *
 * 此前插件用 SettingsSection 的 13/600、技能用裸 h4、工具集干脆没有组标题、
 * 连接器用卡片头代替 —— 同一层信息在四页里长得不一样，连着点就觉得乱。
 */
export function ToolGroupTitle({
  title,
  count,
  action,
}: {
  title: ReactNode;
  count?: number;
  action?: ReactNode;
}) {
  return (
    <div className="mb-2 flex items-center gap-2 px-1">
      <h3 className="text-[length:var(--text-ui)] font-semibold text-[var(--color-text-primary)]">
        {title}
      </h3>
      {count != null && (
        <span className="text-[length:var(--text-tiny)] tabular-nums text-[var(--color-text-faint)]">
          {count}
        </span>
      )}
      {action && <div className="ml-auto flex shrink-0 items-center gap-2">{action}</div>}
    </div>
  );
}

/** 工具页的说明/脚注：统一一档（12 / muted），间距交给父级 space-y。 */
export function ToolHint({ children }: { children: ReactNode }) {
  return (
    <p className="text-[length:var(--text-meta)] leading-relaxed text-[var(--color-text-muted)]">
      {children}
    </p>
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
