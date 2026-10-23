import type { ReactNode } from "react";
import { cx } from "../../lib/cx";

/**
 * 工具类列表的一行（插件 / 技能 / 工具集 / 连接器 / MCP 目录五页共用）。
 *
 * ## 为什么要收敛成一个组件（2026-10-23）
 *
 * 这五页此前各自写了一遍「图标块 + 标题 + 徽标 + 描述 + 右侧动作」的结构，
 * 每份的间距、字号、图标尺寸、动作数量都略有不同，同屏对比时显得杂乱。
 * 更严重的是三条共同病灶：
 *
 *   1. **图标 36px 压过标题**：行高只有 ~44px 时，36px 的色块是整行最重的
 *      元素，视线先落在块上再找文字，读起来是"一张张图"而不是"一条条信息"。
 *      现在统一 28px —— 色彩还在，但不抢文字。
 *   2. **描述 hover 展开导致抖动**：旧写法 `line-clamp-2 group-hover:line-clamp-none`
 *      让卡片在鼠标移上去时变高，网格随之重排，鼠标下方的目标位置跟着跑。
 *      这是"看着很难受但说不清哪里"的典型来源。现在描述恒定单行截断，
 *      完整内容靠 title 属性（悬停原生提示）看。
 *   3. **右侧堆积多个元素**：右边缘同时出现「箭头 + 开关」时，动作区宽度
 *      不一致，多行并排时开关左边缘参差。现在 action 只接一个节点。
 *
 * ## 层次（只有三层，别再往上加）
 *
 *   图标块（彩色） → 标题（primary，13px/500） → 描述（muted，12px，单行）
 * 徽标/元信息挂在标题行尾部，一律 11px faint，不参与主次。
 */
export function ToolListRow({
  icon,
  title,
  description,
  meta,
  action,
  onClick,
  dimmed = false,
  className,
}: {
  icon: ReactNode;
  title: ReactNode;
  /** 一行描述，超出省略。不给则只有标题行（行高自动收窄）。 */
  description?: ReactNode;
  /** 标题行尾部的次要信息（类目、传输方式、工具数…）。 */
  meta?: ReactNode;
  /** 右侧唯一动作：开关 或 箭头 或 按钮，不要叠两个。 */
  action?: ReactNode;
  /** 整行可点（图标区与文字区都响应）。 */
  onClick?: () => void;
  /** 停用态：整体降透明度，但保留可读性。 */
  dimmed?: boolean;
  className?: string;
}) {
  const body = (
    <>
      {icon}
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2">
          <span className="truncate text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
            {title}
          </span>
          {meta && (
            <span className="shrink-0 text-[length:var(--text-tiny)] text-[var(--color-text-faint)]">
              {meta}
            </span>
          )}
        </div>
        {description && (
          <p className="mt-0.5 truncate text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
            {description}
          </p>
        )}
      </div>
      {action && <div className="flex shrink-0 items-center gap-1">{action}</div>}
    </>
  );

  const shell = cx(
    "group flex items-center gap-2.5 rounded-[var(--radius-lg)] px-2.5 py-2",
    "transition-colors duration-[var(--duration-fast)] ease-[var(--ease-out)]",
    "bg-[var(--color-bg-card)]",
    dimmed && "opacity-60",
    className,
  );

  if (onClick) {
    return (
      <button
        type="button"
        onClick={onClick}
        className={cx(shell, "w-full text-left hover:bg-[var(--color-bg-card-hover)]")}
      >
        {body}
      </button>
    );
  }
  return <div className={cx(shell, "hover:bg-[var(--color-bg-card-hover)]")}>{body}</div>;
}
