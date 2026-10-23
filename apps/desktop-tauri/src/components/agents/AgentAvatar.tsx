import { cx } from "../../lib/cx";
import { toneClass, type IconTone } from "../../lib/categoryTone";
import type { AgentColorTheme } from "./types";

/**
 * 专家头像 = 「首汉字 / 首字母」色块（2026-10-23 取代 emoji 头像）。
 *
 * ## 为什么不用 emoji
 *
 * 旧实现是 18 个 emoji 的选择器（`AVATARS`）。问题不在审美，在**不可控**：
 *   1. 字形由系统字体决定 —— Win10 用 Segoe UI Emoji、Win11 用 Fluent Emoji，
 *      同一份数据在两个系统上是两套画风，且都不受应用主色影响；
 *   2. 尺寸跟随字号，1em 的字形里实际视觉大小各不相同（🤖 比 ⚡ 大一圈），
 *      网格里九个头像大小不齐；
 *   3. 颜色写死在字体里，不跟主题、不跟卡片色板；
 *   4. 与旁边全站统一的 lucide 描边图标不是一个视觉体系。
 *
 * ## 为什么是「首字」而不是图标
 *
 * 头像要表达的是**身份**（这是谁），不是**类目**（这是什么）。专家数量无上界，
 * 图标集无法为每个专家挑一个"准确"的图标；而首字天然唯一、零维护，且在
 * 中文里一个字的信息密度远高于一个泛化图标（「代」比 ⚡ 更能说明它是代码专家）。
 *
 * 色调仍走 `colorTheme`（卡片顶条 / chip 用的是同一份），保证一个专家的
 * 头像、顶条、标签在同一色相族里；实际色值来自 --color-tone-* 令牌，
 * 不写字面色值，亮暗两套主题自动跟随。
 */

/** 卡片色板键 → 图标色调。PALETTES 的 8 个键在这里收敛到 8 档色相。 */
const THEME_TONE: Record<AgentColorTheme, IconTone> = {
  violet: "orange", // 历史键名，现值是陶土红
  rose: "rose",
  cyan: "cyan",
  emerald: "green",
  amber: "amber",
  teal: "blue", // 深青偏冷，归蓝档
  indigo: "green", // 历史键名，现值是橄榄绿
  fuchsia: "violet",
};

/**
 * 取头像字：中日韩字符取首字，其余取首字符大写。
 * 用 `Array.from` 而不是 `s[0]` —— emoji / 生僻字是代理对，`s[0]` 会切出半个码元。
 */
export function avatarInitial(name?: string | null): string {
  const s = (name ?? "").trim();
  if (!s) return "?";
  const first = Array.from(s)[0] ?? "?";
  // 汉字 / 假名 / 谚文：原样；拉丁等：大写
  return /[\u3400-\u9fff\u3040-\u30ff\uac00-\ud7af]/.test(first)
    ? first
    : first.toUpperCase();
}

export function AgentAvatar({
  name,
  colorTheme = "violet",
  tone: toneOverride,
  size = "md",
  className,
}: {
  name?: string | null;
  colorTheme?: AgentColorTheme;
  /** 直接指定色调，优先于 colorTheme。给没有 colorTheme 字段的目录
   *  （如内置专家 ExpertDef）按类目推导用。 */
  tone?: IconTone;
  size?: "sm" | "md" | "lg";
  className?: string;
}) {
  const tone = toneOverride ?? THEME_TONE[colorTheme] ?? "slate";
  return (
    <span
      aria-hidden
      className={cx("agent-avatar", `cat-icon cat-icon--${size}`, toneClass(tone), className)}
    >
      {avatarInitial(name)}
    </span>
  );
}
