import type { ReactNode } from "react";

import { toneClass, toneForCategory, type IconTone } from "../../lib/categoryTone";

/**
 * 工具类列表卡的图标色块（插件 / 技能 / 工具集 / MCP 四页共用）。
 *
 * 抽出来的唯一目的是让四页**必然**一致：改一次，四处同时生效，不会再漂移。
 *
 * ## 为什么从「单一主色」改成「按类目上色」（2026-10-23）
 *
 * 2026-10-05 曾把四档语义色删掉、统一成主色，当时的三条理由里，前两条
 * 今天依然成立、且必须继续遵守：
 *
 *   1. **语义四色不能当分类色用。** info/success/warning 在本项目里已经被
 *      状态语义占用（安全扫描结果、启停状态），拿去做分类会语义串台。
 *   2. **色相必须走 token。** tsx 里写字面色值会被 check-design-guard 拦下，
 *      且换主题时不跟随。
 *
 * 但第 3 条「分类辨识靠位置不靠颜色」在四页共用网格的场景下不成立：
 * 插件页里 MCP / Tools 两类卡片是**混排**的，技能页里 7 个分类也是混排的，
 * 位置不承载分类信息 —— 于是所有图标都是同一个绿，色相通道完全闲置。
 *
 * 解法是**新增一套与语义四色物理隔离的身份色**（--color-tone-*，8 档），
 * 只用于图标。它不与状态语义抢名字，也不参与文字着色，两条旧纪律都保住了。
 *
 * 底色规则（2026-10-23 二次修正）：**底是中性的主题实底**（--color-bg-tertiary），
 * **图标才是饱和实色**。中间曾试过「图标色 14% 淡染当底」，那样底色和图标
 * 同色系，整块像蒙了一层同色雾，彩色被自己稀释，网格里十几张卡糊成同一调
 * 浅彩 —— 色相该只出现在图形上，底色只交代"这里有个图形块"。
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
    `cat-icon cat-icon--${size}`,
    toneClass(resolved),
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
