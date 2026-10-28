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
 * ## 层次（四个区，靠结构与间距分开，不是靠"位置顺序"）
 *
 *   ① 标题区   [图标] 标题 · 元信息 ………………… 主操作（右上，与标题对齐）
 *   ② 描述区   一句说明（muted，12px，单行截断）
 *   ③ 附属区   chips / 工具名等（可选，用 1px 分隔线与上面切开）
 *
 * 2026-10-28 重构：此前图标、标题、描述、动作全在一条 `items-center` 的水平线上，
 * 主操作（开关）随卡片高度垂直居中，和标题/描述混成一层 —— 卡片一多就分不出
 * "这块说的是什么、哪个是能点的"。现在：
 *   · 外层改 `items-start`，图标与标题顶对齐，长描述不再把图标顶到中间；
 *   · 主操作 `self-start` 固定贴在标题行右端，与标题同一基线；
 *   · 附属信息走 `footer` 槽，与主体之间有一条极淡的分隔线（第三种区隔手段，
 *     前两种是间距和子容器）。
 */
export function ToolListRow({
  icon,
  title,
  description,
  meta,
  action,
  footer,
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
  /** 右侧主操作：开关 / 箭头，与标题同基线。 */
  action?: ReactNode;
  /** 附属区：chips、工具名、次级动作。会在上面画一条分隔线。 */
  footer?: ReactNode;
  /** 整行可点（图标区与文字区都响应）。 */
  onClick?: () => void;
  /** 停用态：整体降透明度，但保留可读性。 */
  dimmed?: boolean;
  className?: string;
}) {
  const body = (
    <>
      <div className="shrink-0 self-start">{icon}</div>
      <div className="min-w-0 flex-1">
        {/* ① 标题区：标题 + 元信息 */}
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
        {/* ② 描述区 */}
        {description && (
          <p className="mt-1 truncate text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
            {description}
          </p>
        )}
        {/* ③ 附属区：与主体用一条分隔线 + 上间距切开 */}
        {footer && (
          <div className="mt-2 border-t border-[var(--color-border-primary)] pt-2">
            {footer}
          </div>
        )}
      </div>
      {action && (
        <div className="ml-1 flex shrink-0 items-center gap-1 self-start">{action}</div>
      )}
    </>
  );

  const shell = cx(
    "group flex items-start gap-2.5 rounded-[var(--radius-lg)] px-2.5 py-2",
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
