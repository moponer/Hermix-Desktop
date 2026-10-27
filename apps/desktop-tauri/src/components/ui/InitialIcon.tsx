import { initialOf, toneOf } from "../../lib/initialIcon";

/**
 * 列表行图标：名字首字（汉字 / 字母 / 数字）+ 稳定身份色。
 *
 * 形态走 `.cat-icon`（中性底 + 彩字）——这是仓库里**字母**的既定形态：
 * 8 档色相里没有任何单一前景能压在实心底上同时过对比度，所以字母不铺
 * 实底。尺寸只有 sm(28 列表行) / md(36 卡片) 两档，与 `.cat-icon` 对齐。
 *
 * 用法：`<InitialIcon name={skill.display_name || skill.name} />`
 */
export function InitialIcon({
  name,
  size = "sm",
  className,
}: {
  name: string | null | undefined;
  size?: "sm" | "md";
  className?: string;
}) {
  const tone = toneOf(name);
  const cls = ["cat-icon", `cat-icon--${size}`, `cat-icon--${tone}`, className ?? ""]
    .filter(Boolean)
    .join(" ");
  return (
    <span aria-hidden className={cls}>
      <span className="initial-glyph">{initialOf(name)}</span>
    </span>
  );
}
