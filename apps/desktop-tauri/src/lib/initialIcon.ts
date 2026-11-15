/**
 * 列表行「首字图标」的取名与选色。
 *
 * 为什么不按分类给图标：一屏十几行同一个类目就十几枚一模一样的图标，
 * 图标不携带任何信息，只剩装饰。改成「名字首字」后，每一行都能被扫到。
 *
 * 两条规则：
 *   1. 首字：汉字取第一个汉字，拉丁取第一个字母（大写），数字保留；
 *      前导 emoji / 标点 / 空白一律跳过（「【新】xxx」→ x）。
 *   2. 色相：名字的稳定哈希 → 8 档身份色之一。同一个名字永远同色，
 *      不随排序/筛选变化；色相只做区分，不表达状态。
 *
 * 字母与图标是两种形态（见 icon-tone.css）：字母走 `.cat-icon`（中性底 +
 * 彩字），实心图标走 `.cat-tile`。字母用实心底会在 8 档色相里找不到
 * 统一的前景（对比度不齐），所以这里不带 tile。
 */

export const TONES = ["slate", "blue", "cyan", "green", "amber", "orange", "rose", "violet"] as const;

export type InitialTone = (typeof TONES)[number];

const HAN = /\p{Script=Han}/u;
const LATIN = /[A-Za-z]/;
const DIGIT = /[0-9]/;

/** 取名字的首字：汉字 / 拉丁字母（大写）/ 数字；都没有则退化成一个中点。 */
export function initialOf(name: string | null | undefined): string {
  const s = (name ?? "").trim();
  for (const ch of s) {
    if (HAN.test(ch)) return ch;
    if (LATIN.test(ch)) return ch.toUpperCase();
    if (DIGIT.test(ch)) return ch;
  }
  return "·";
}

/** 稳定选色：FNV-1a 的 32 位变体，只取模 8；同名必然同色。 */
export function toneOf(name: string | null | undefined): InitialTone {
  const s = (name ?? "").trim();
  let h = 0x811c9dc5;
  for (let i = 0; i < s.length; i += 1) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 0x01000193);
  }
  return TONES[Math.abs(h) % TONES.length];
}
