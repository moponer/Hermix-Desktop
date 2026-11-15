import type { ReactNode } from "react";

import { cx } from "../../lib/cx";
import { type IconTone } from "../../lib/categoryTone";

/**
 * 列表行左侧的实心色块图标（2026-10-23）。
 *
 * 形态：**实心身份色底 + 反白字形**（共享层 `.cat-tile`）。
 * 与 `.cat-icon`（中性底 + 彩色字形）是两种形态，不是同一件事的深浅版本：
 * 色相落在**块**上还是落在**字**上，扫视时的落点完全不同。
 *
 * 为什么要有这层封装而不是直接写 className：
 *  1. `cat-tile cat-tile--sm cat-tile--blue` 这串要在几十个列表行里重复，
 *     手写迟早出现"某个页面忘了加尺寸档"的漂移；
 *  2. 字形尺寸必须跟着块尺寸走（sm 28px 配 14px 字形，md 36px 配 16px），
 *     封装里定死，调用方不用判断；
 *  3. 反白前景由 `--color-on-tone` 供给，**分主题**（浅色白字 / 深色深墨），
 *     写死颜色会在深色主题下静默不达标 —— 这一条尤其不能由调用方承担。
 *
 * 色相只表达「这是什么」（类目身份），不表达状态；状态语义仍走
 * `--color-success / warning / error / info`，两者不复用。
 */
export function IconTile({
  icon,
  tone = "slate",
  size = "sm",
  className,
}: {
  /** 字形节点，通常是 `<Foo className="icon-md" />`。 */
  icon: ReactNode;
  /** 身份色相，来自 8 档 `--color-tone-*`。 */
  tone?: IconTone;
  /** 块尺寸：sm=28（列表行）· md=36（卡片）· lg=48（页头 / 空态）。 */
  size?: "sm" | "md" | "lg";
  className?: string;
}) {
  return (
    <span
      aria-hidden
      className={cx("cat-tile", `cat-tile--${size}`, `cat-tile--${tone}`, className)}
    >
      {icon}
    </span>
  );
}
