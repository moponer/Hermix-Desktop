/**
 * 拼接 class 名，丢掉假值。
 *
 * 刻意不用 `clsx` + `tailwind-merge`：那要装依赖，而这个项目不带 UI 库
 * （抄 cc-haha `desktop/src/lib/cx.ts` 的同一条理由）。代价是 `cx` **不
 * 解析冲突的 Tailwind 工具类** —— 给一个已经带 `px-2` 的变体再传
 * `className="px-4"`，两个都会留在 class 列表里，谁赢由生成样式表里的排序
 * 决定，不由这里的顺序决定。调用方需要覆盖同一个工具类时，给组件加变体，
 * 不要在外部覆盖。
 */
export function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}
