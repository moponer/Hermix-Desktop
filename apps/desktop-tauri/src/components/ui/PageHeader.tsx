import type { ElementType, ReactNode } from "react";

import { cx } from "../../lib/cx";

export interface PageHeaderProps {
  /** 语义图标（lucide，尺寸由内部约束为 icon-lg） */
  icon?: ReactNode;
  title: string;
  /** 计数徽标；不传则不渲染 */
  count?: number | string;
  /** 中部插槽：视图切换（SegmentedControl）等 */
  children?: ReactNode;
  /** 右侧动作区；一屏至多一个 accent 实底按钮 */
  actions?: ReactNode;
  /** 标题标签层级；页面内其余标题从它顺级排（h1 → h2 → h3） */
  as?: ElementType;
  className?: string;
}

/**
 * 页面级统一页头（R1-3）：h-56，左定位 / 中切换 / 右动作。
 * 页头下不画分隔线 —— 层级靠下方内容的组间距（24px）表达。
 * 现存的 4 种页头写法（h1 / h2+text-title / h2+text-lg / uppercase 胶囊）
 * 全部收敛到这里。
 */
export function PageHeader({
  icon,
  title,
  count,
  children,
  actions,
  as: Tag = "h1",
  className,
}: PageHeaderProps) {
  return (
    <header
      className={cx(
        "flex h-14 shrink-0 items-center gap-3 px-6",
        className,
      )}
    >
      {icon && (
        <span
          aria-hidden
          /* 墨与纸 v4：图标底座中性化（色相退出组件层） */
          className="grid size-[30px] shrink-0 place-items-center rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)]"
        >
          {icon}
        </span>
      )}
      <Tag className="text-[length:var(--text-title)] font-semibold leading-[var(--leading-heading)] text-[var(--color-text-primary)]">
        {title}
      </Tag>
      {count != null && count !== "" && (
        <span className="rounded-full border-[var(--color-border-primary)] bg-[var(--color-bg-card)] px-2 py-px font-mono text-[length:var(--text-meta)] text-[var(--color-text-faint)]">
          {count}
        </span>
      )}
      {children != null && <div className="ml-4 flex items-center gap-2">{children}</div>}
      {actions != null && <div className="ml-auto flex shrink-0 items-center gap-2">{actions}</div>}
    </header>
  );
}
