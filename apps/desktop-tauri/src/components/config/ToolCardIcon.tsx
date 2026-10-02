import type { ReactNode } from "react";

/**
 * 工具类列表卡的图标色块（插件 / 技能 / 连接器 / 工具集 四页共用）。
 *
 * 这四页原先各写一遍 `<div className="w-9 h-9 rounded-lg bg-bg-tertiary
 * … text-text-secondary">` —— 同一行代码抄了四份，结果四页的卡片长得
 * 一模一样且**图标全是灰的**：底是 bg-tertiary、字是 text-secondary，
 * 两档灰压在一起，扫一列卡片时图标完全不起视觉锚点作用，用户抱怨"没有
 * 层次、图标没有颜色"说的正是这里。
 *
 * 抽出来是为了让四页**必然**一致：改一次，四处同时生效，不会再漂移。
 * 颜色本身按语义给（见各页 `TONE_FOR_*`），不是随机上色——蓝色=MCP
 * 外部服务、强调色=内置工具族、绿色=技能（可执行能力）、琥珀=本地/文件
 * 资源。扫一眼就知道这一片是什么。
 */
export type ToolCardTone = "accent" | "info" | "success" | "warning";

const TONE_CHIP: Record<ToolCardTone, { bg: string; fg: string }> = {
  // 强调色档用 accent-on-text（accent 的浅染）而不是 accent-soft：
  // 后者在浅色主题下几乎与白底同色，色块会"消失"。
  accent: { bg: "var(--color-accent-on-text)", fg: "var(--color-accent)" },
  info: { bg: "var(--color-info-soft)", fg: "var(--color-info)" },
  success: { bg: "var(--color-success-soft)", fg: "var(--color-success)" },
  warning: { bg: "var(--color-warning-soft)", fg: "var(--color-warning)" },
};

export function ToolCardIcon({
  children,
  tone = "accent",
  interactive = false,
}: {
  children: ReactNode;
  tone?: ToolCardTone;
  /** 原卡片里图标块可点（技能的图标点开即打开技能）。 */
  interactive?: boolean;
}) {
  const t = TONE_CHIP[tone];
  return (
    <span
      aria-hidden
      className={`flex h-9 w-9 shrink-0 items-center justify-center rounded-[var(--radius-md)] transition-colors ${
        interactive ? "hover:brightness-95" : ""
      }`}
      style={{ background: t.bg, color: t.fg }}
    >
      {children}
    </span>
  );
}

/** 按插件/技能分类名挑一个稳定色调（同名永远同色，跨会话一致）。 */
export function toneForCategory(category: string): ToolCardTone {
  const c = category.toLowerCase();
  if (c.includes("mcp") || c.includes("remote") || c.includes("服务")) return "info";
  if (c.includes("skill") || c.includes("技能")) return "success";
  if (c.includes("file") || c.includes("本地") || c.includes("文件")) return "warning";
  return "accent";
}
