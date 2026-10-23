import type { ReactNode } from "react";

import { toneClass, toneForCategory, type IconTone } from "../../lib/categoryTone";

/**
 * 工具类列表卡的图标色块（插件 / 技能 / 工具集 / MCP 四页共用）。
 *
 * 抽出来的唯一目的是让四页**必然**一致：改一次，四处同时生效，不会再漂移。
 *
 * ## 形态：实心底 + 反白字形（2026-10-23）
 *
 * 原先是「中性实底 + 饱和图标色」（`.cat-icon`）。现在改为 `.cat-tile`
 * ——**底是身份色，字是反白**：色相落在**块**上而不是细字上。
 *
 * 换形态的理由：
 *   1. **小尺寸下彩字太薄**。28px 的块里放 12px 细描边字形，色相被自己
 *      稀释掉（这正是 2026-10-23 第一次修正想解决的：淡染底 + 彩字像蒙了
 *      一层同色雾）。实心底把色相坐实，雾没有了。
 *   2. **扫视落点不同**。一屏十几行彩字 = 一堆细线；一排实心块 = 一排色标，
 *      「这是什么」在块上就答完了，不必读完文字。
 *
 * ## 为什么不改 `.cat-icon` 本身
 *
 * `.cat-icon` 是「中性底 + 彩字」，语义不同、被多处消费，改它等于把现有
 * 视觉一起推翻。两种形态并存，各用各的（`.cat-tile` 见共享层 icon-tone.css）。
 *
 * ## 对比度（WCAG 1.4.11 非文本 ≥3:1）
 *
 * 字形前景走 `--color-on-tone`，**分主题**：浅色主题白字压在 8 档 tone 上是
 * 3.17~4.82:1（全部达标）；深色主题的 tone 是"暗底提亮档"，白字只有
 * 2.0~2.6:1（不达标），必须用深墨（6.99~9.14:1）。写死白色会在暗色主题
 * 下静默失效 —— 所以前景色由令牌供给，组件不碰色值。
 */
export function ToolCardIcon({
  children,
  tone,
  category,
  size = "sm",
  interactive = false,
  onClick,
  title,
  "aria-label": ariaLabel,
}: {
  children: ReactNode;
  /** 显式指定色调；不给则按 category 推导，再不给则 slate。 */
  tone?: IconTone;
  /** 类目名（自由文本），用于推导色调。 */
  category?: string | null;
  /** 尺寸。列表行默认 sm(28px)：36px 的块在 44px 高的行里会压过标题，
   *  这是列表"显得拥挤"的主因之一。md(36) 留给网格卡片。 */
  size?: "sm" | "md";
  /** hover 放大（卡片图标可点时的轻反馈）。 */
  interactive?: boolean;
  /** 传入则在按钮语义下渲染（图标点开即打开技能 / 详情）。 */
  onClick?: () => void;
  title?: string;
  "aria-label"?: string;
}) {
  const resolved = tone ?? toneForCategory(category);
  const cls = [
    `cat-tile cat-tile--${size}`,
    toneClass(resolved, "cat-tile"),
    onClick || interactive ? "m-icon-hover cursor-pointer" : "",
  ].join(" ");
  if (onClick) {
    return (
      <button
        type="button"
        onClick={onClick}
        title={title}
        aria-label={ariaLabel}
        className={cls}
      >
        {children}
      </button>
    );
  }
  return (
    <span aria-hidden className={cls}>
      {children}
    </span>
  );
}
