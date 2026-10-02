/**
 * 强调色策展清单（L3.5）。
 *
 * 为什么是策展清单而不是自由取色器：`--color-on-accent`（压在 accent 实色
 * 上的文字色）**无法用 color-mix 派生**——它必须随 accent 明度在浅/深文字
 * 间切换，自由取色时没有正确答案。清单里每一项都预先定好了 on 色，注入时
 * 一次给 `--color-accent` + `--color-on-accent` 两个变量，其余派生家族
 * （hover / soft / soft-strong）由 base.css 的 color-mix 级联自动跟随。
 *
 * 色值选取：全部取明度足够低的实色，on 一律白字，保证 accent 实色块上的
 * 文字对比度 ≥ 4.5:1（WCAG AA normal）。**不收录浅色**——浅色 accent 会
 * 需要深字，与本项目"纸墨"底色的观感冲突，且要维护两套 on 规则。
 *
 * 末项 `null` = 跟随主题默认（移除注入），对应 base.css 的
 * `--color-accent: var(--color-accent-base)` 供给。
 */
export interface AccentChoice {
  /** 展示名（中文） */
  label: string;
  /** 注入值；null = 跟随主题 */
  value: string | null;
  /** 压在 accent 实色上的文字色 */
  on: string;
  /** 色板圆点的呈现色；跟随主题时取中性色 */
  swatch: string;
}

export const ACCENT_PALETTE: AccentChoice[] = [
  { label: "跟随主题", value: null, on: "#ffffff", swatch: "var(--color-bg-tertiary)" },
  { label: "松绿", value: "#2f7d54", on: "#ffffff", swatch: "#2f7d54" },
  { label: "青碧", value: "#0e7c86", on: "#ffffff", swatch: "#0e7c86" },
  { label: "靛蓝", value: "#2563eb", on: "#ffffff", swatch: "#2563eb" },
  { label: "紫棠", value: "#7c3aed", on: "#ffffff", swatch: "#7c3aed" },
  { label: "赭橙", value: "#c2620a", on: "#ffffff", swatch: "#c2620a" },
  { label: "绛红", value: "#c0392b", on: "#ffffff", swatch: "#c0392b" },
];

/** accent hex → 配套 on 色。清单外（历史遗留 / 手改 localStorage）回落白。 */
export function onAccentFor(accent: string): string {
  return ACCENT_PALETTE.find((a) => a.value === accent)?.on ?? "#ffffff";
}

/** 展示用的 HEX 大写短写；跟随主题时返回占位串。 */
export function accentLabel(accent: string | null): string {
  if (!accent) return "跟随主题";
  return accent.toUpperCase();
}
