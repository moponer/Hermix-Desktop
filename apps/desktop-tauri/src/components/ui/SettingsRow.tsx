import type { ReactNode } from "react";

import { cx } from "../../lib/cx";

export interface SettingsRowProps {
  title: ReactNode;
  description?: ReactNode;
  /** 右侧控件：Switch / Select / Button / 色板，由调用方给。 */
  control?: ReactNode;
  /** 左侧图标块：实心身份色块（`IconTile`）。不给则整行只有文字。 */
  icon?: ReactNode;
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
 *
 * `icon` 插槽（2026-10-23 新增）：左侧可放一个实心身份色块（`IconTile`）。
 * 判据是**这一行是不是一项功能/能力**——是则给块（色块承担"这是什么"），
 * 只是数值参数的行不给（参数行配图标等于几个同义图形，信息量为零）。
 */
export function SettingsRow({
  title,
  description,
  control,
  icon,
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
        // 行高从 14px(py-3.5) 收回 16px(py-4)：设置行是「功能/能力」行，
        // 半档 14px 让它与 12/16 的卡片内边距对不齐。
        "flex w-full items-center gap-4 px-5 py-4 text-left",
        // className 曾经写成 `interactive && className`：不可点的行传进来的
        // 布局类会被静默丢弃，于是同一组设置行的 padding/对齐按"是否可点"
        // 分叉。外部传入的类必须无条件生效。
        className,
      )}
    >
      {icon && <span className="shrink-0">{icon}</span>}
      <div className="min-w-0 flex-1">
        <div
          className={cx(
            "text-[length:var(--text-body)] font-medium text-[var(--color-text-primary)]",
            interactive && "cursor-pointer",
          )}
        >
          {title}
        </div>
        {description && (
          <div className="mt-1 text-[length:var(--text-ui)] leading-relaxed text-[var(--color-text-muted)]">
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
  className?: string;
  children: ReactNode;
}

/** 分组卡片：圆角容器包若干行，行间靠间距分层（全组无描边线）。 */
export function SettingsGroup({
  title,
  description,
  className,
  children,
}: SettingsGroupProps) {
  return (
    <section className={cx("space-y-2", className)}>
      {(title || description) && (
        <header className="px-1">
          {title && (
            <h3 className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-secondary)]">
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
      {/* 卡片不画描边：面靠底色差（白卡压在灰底）+ 极淡投影抬起。
          1px 边框会把每张卡读成「输入框」，满屏盒子感正是廉价感的来源。 */}
      <div className="overflow-hidden rounded-[var(--radius-lg)] bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)]">
        {children}
      </div>
    </section>
  );
}
