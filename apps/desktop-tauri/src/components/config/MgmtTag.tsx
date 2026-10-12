import type { ReactNode } from "react";

/**
 * 管理界面（插件 / 连接器 / 工具集 / 技能 / MCP 目录）共用的分类 / 状态药丸。
 *
 * 抽出来的唯一目的：让五处标签**必然**一致，且彻底淘汰旧的
 * `uppercase tracking-wider` 样式——那种全大写 + 字距拉开的胶囊是 AI 生成 UI
 * 的典型廉价感来源（impeccable 的 side-tab 同级反模式）：它把"分类"伪装成
 * "系统元标签"，在中文界面里尤其突兀。
 *
 * 新契约：小写（中文本就没大小写）、`rounded-full` 柔和药丸、用状态语义色而非
 * 全大写来传达层级。tone 直接映射项目既有的 soft 令牌，明暗两套主题自动适配。
 */
type MgmtTagTone = "neutral" | "accent" | "success" | "warning" | "error";

const TONE: Record<MgmtTagTone, string> = {
  neutral: "bg-[color-mix(in_srgb,var(--color-text-primary)_5%,transparent)] text-text-muted",
  accent: "bg-[var(--color-accent-on-text)] text-[var(--color-accent)]",
  success: "bg-[var(--color-success-soft)] text-[var(--color-success)]",
  warning: "bg-[var(--color-warning-soft)] text-[var(--color-warning)]",
  error: "bg-[var(--color-error-soft)] text-[var(--color-error)]",
};

export function MgmtTag({
  children,
  tone = "neutral",
  className = "",
}: {
  children: ReactNode;
  tone?: MgmtTagTone;
  className?: string;
}) {
  return (
    <span
      className={`inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-[length:var(--text-tiny)] font-medium leading-none ${TONE[tone]} ${className}`}
    >
      {children}
    </span>
  );
}
