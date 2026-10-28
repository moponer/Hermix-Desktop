import { initialOf } from "../../lib/initialIcon";

/**
 * 列表行图标：名字首字（汉字 / 字母 / 数字）+ **中性单色**。
 *
 * ## 为什么不再按名字上彩（2026-10-28）
 *
 * 旧版用名字哈希从 8 档身份色里选色（同一个名字永远同色）。想法是"色相
 * 帮助扫视"，但实测四个配置页（插件 / 技能 / 工具集 / 连接器）后用户明确
 * 反馈「图标色、字体色、开关色像大杂烩」：哈希色**不携带任何信息**——
 * 颜色既不表达类目也不表达状态，一屏十几行就是十几种随机颜色，只剩噪音。
 * 与左侧导航 2026-10-23 的裁决同一条原则：颜色只留给真正表达含义的地方
 * （状态语义色 / 强调色），身份识别交给首字本身。
 *
 * 形态仍是 `.cat-icon`（中性底 + 单色字形），尺寸 sm(28) / md(36) 两档。
 * `toneOf` 哈希保留在 lib 里（测试与未来真需要身份色的场景仍在用）。
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
  const cls = ["cat-icon", `cat-icon--${size}`, "cat-icon--slate", className ?? ""]
    .filter(Boolean)
    .join(" ");
  return (
    <span aria-hidden className={cls}>
      <span className="initial-glyph">{initialOf(name)}</span>
    </span>
  );
}
