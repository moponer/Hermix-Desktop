import type { ReactNode } from "react";

import { cx } from "../../lib/cx";

export interface SettingsRowProps {
  title: ReactNode;
  description?: ReactNode;
  /** 右侧控件：Switch / Select / Button / 色板，由调用方给。 */
  control?: ReactNode;
  /** 整行可点（如点击标题切开关）时的处理器；有 control 时慎用。 */
  onClick?: () => void;
  disabled?: boolean;
  className?: string;
  children?: ReactNode;
}

/**
 * 设置行：左标题+说明 / 右控件。
 *
 * 竖排而非横排：中文标题普遍 6–12 字，横排在 720px 栏宽里会把控件挤出
 * 视觉基线，用户扫一列开关时高低不齐。竖排让所有控件落在同一条右边界。
 */
export function SettingsRow({
  title,
  description,
  control,
  onClick,
  disabled,
  className,
  children,
}: SettingsRowProps) {
  const interactive = !!onClick && !disabled;
  const Tag = interactive ? "button" : "div";

  return (
    <Tag
      {...(interactive
        ? { type: "button" as const, onClick, disabled }
        : {})}
      className={cx(
        "flex w-full items-center gap-4 px-4 py-3 text-left",
        interactive &&
          "transition-colors hover:bg-[var(--color-bg-card-hover)] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-[var(--color-accent)]",
        className,
      )}
    >
      <div className="min-w-0 flex-1">
        <div
          className={cx(
            "text-[length:var(--text-ui)] text-[var(--color-text-primary)]",
            interactive && "cursor-pointer",
          )}
        >
          {title}
        </div>
        {description && (
          <div className="mt-0.5 text-[length:var(--text-meta)] leading-relaxed text-[var(--color-text-muted)]">
            {description}
          </div>
        )}
        {children}
      </div>
      {control && <div className="shrink-0">{control}</div>}
    </Tag>
  );
}

export interface SettingsGroupProps {
  title?: ReactNode;
  description?: ReactNode;
  /** 行元素之间自动加分隔线；行自己不必管。 */
  divided?: boolean;
  className?: string;
  children: ReactNode;
}

/** 分组卡片：圆角容器包若干行，行间 1px 分隔线。 */
export function SettingsGroup({
  title,
  description,
  divided = true,
  className,
  children,
}: SettingsGroupProps) {
  return (
    <section className={cx("space-y-2", className)}>
      {(title || description) && (
        <header className="px-1">
          {title && (
            <h3 className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
              {title}
            </h3>
          )}
          {description && (
            <p className="mt-0.5 text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
              {description}
            </p>
          )}
        </header>
      )}
      <div
        className={cx(
          "overflow-hidden rounded-[var(--radius-lg)] border border-[var(--color-border-primary)] bg-[var(--color-bg-card)]",
        )}
      >
        {divided ? <DividedRows>{children}</DividedRows> : children}
      </div>
    </section>
  );
}

/** 给直接子元素（非 SettingsRow 的裸 div）也补上分隔线。 */
function DividedRows({ children }: { children: ReactNode }) {
  // 过滤条件渲染留下的 false/null：留着会给那一行画一条分隔线，
  // 视觉上像"下面还有内容但没渲染出来"。
  const items = (Array.isArray(children) ? children.flat() : [children]).filter(
    (c) => c !== null && c !== undefined && c !== false,
  );
  return (
    <>
      {items.map((child, i) => (
        <div
          key={i}
          className={cx(i > 0 && "border-t border-[var(--color-border-subtle)]")}
        >
          {child}
        </div>
      ))}
    </>
  );
}
