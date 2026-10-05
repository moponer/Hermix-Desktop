import type { ReactNode } from "react";

/**
 * 工具类列表卡的图标色块（插件 / 技能 / 工具集 / MCP 四页共用）。
 *
 * 抽出来的唯一目的是让四页**必然**一致：改一次，四处同时生效，不会再漂移。
 * 早先四页各写一遍 `bg-bg-tertiary … text-text-secondary`，结果图标全是灰的，
 * 两档灰压在一起不起视觉锚点作用——「没有层次、图标没有颜色」说的正是这里。
 *
 * ## 为什么只有一种色调（2026-10-05）
 *
 * 本组件此前有 accent / info / success / warning 四档语义色，由 `toneForCategory`
 * 按「MCP / 技能 / 文件」分派。**已删除**，改为统一主色，理由三条：
 *
 * 1. **色相在项目里是受管控的。** themes/dark.css 把 Tailwind 原生色槽全部
 *    重定向到语义色（`red-400`→`--color-error`、`emerald-400`→`--color-success`
 *    …），所谓「给每个分类发一个颜色」在项目体系里没有立足点。
 * 2. **四档语义色与分类不对应。** `toneForCategory` 按分类名匹配
 *    「mcp / 技能 / file」，但同一个分类在不同页面含义会变；而 info/success/
 *    warning 在本项目里已经被状态语义占用（安全扫描结果、启停状态），
 *    再拿来当分类色会造成语义串台。
 * 3. **分类辨识靠位置不靠颜色。** 三个 tab 各自只展示一个维度，分类由分组
 *    标题承担；卡片颜色若按分类变化，只会在同一屏引入无意义差异。
 *
 * 统一到主按钮那一对 token 后，图标色 = 全站主按钮色，亮色松墨绿、夜墨青绿，
 * 天然与主题一致，不需要在此处做任何主题分支。
 */
export function ToolCardIcon({
  children,
  interactive = false,
}: {
  children: ReactNode;
  /** 原卡片里图标块可点（技能的图标点开即打开技能）。 */
  interactive?: boolean;
}) {
  return (
    <span
      aria-hidden
      className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-[var(--radius-md)] transition-transform ${
        interactive ? "hover:scale-105" : ""
      }`}
      style={{
        background: "var(--color-btn-primary-bg)",
        color: "var(--color-btn-primary-text)",
        boxShadow: "var(--shadow-button-primary)",
      }}
    >
      {children}
    </span>
  );
}
