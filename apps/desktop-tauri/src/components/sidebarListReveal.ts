/**
 * 侧栏列表的归集（折叠 / 展开 / 查看全部）规则。
 *
 * 单独成模块而非留在 Sidebar 里：这是纯计算，有两个消费场景（对话分组、
 * 每个项目内嵌列表）和一组必须锁死的边界，放进 2000 行的组件里既测不到
 * 也容易被改坏。
 */

/** 折叠态显示条数。超过它才出现"查看更多"。 */
export const LIST_PREVIEW_COUNT = 5;
/** 就地展开的上限。超过它必须走"查看全部"弹窗，避免侧栏被无限撑长。 */
export const LIST_EXPAND_CAP = 20;

/**
 * 列表归集的三种呈现方式。
 *
 * 三层缺一不可：如果只有"折叠/展开"两态，当总数超过展开上限时会卡死
 * ——展开后按钮既不能是"收起"（一点就从上限跳回折叠态，中间那些白展开了），
 * 也不能继续是"查看更多"（已经是上限，点了没反应）。`all` 就是那个出口。
 */
export type ListReveal = "preview" | "expanded" | "all";

/**
 * 按总数决定列表当前该显示多少条。
 *
 * - ≤ 折叠阈值：全量显示，无按钮
 * - ≤ 展开上限：折叠显示前 N 条，可"查看更多"就地展开
 * - > 展开上限：就地展开只到上限，其余走"查看全部"弹窗
 */
export function resolveVisibleCount(total: number, reveal: ListReveal): number {
  if (total <= LIST_PREVIEW_COUNT) return total;
  if (reveal === "all") return total;
  return reveal === "expanded" ? Math.min(total, LIST_EXPAND_CAP) : LIST_PREVIEW_COUNT;
}

/** 列表底部那一个按钮当前该呈现什么。null = 不需要按钮。 */
export type ListRevealAction =
  | { kind: "expand"; hidden: number }
  | { kind: "collapse" }
  | { kind: "openPicker"; hidden: number };

export function resolveRevealAction(
  total: number,
  reveal: ListReveal,
): ListRevealAction | null {
  if (total <= LIST_PREVIEW_COUNT) return null;
  if (reveal === "preview") {
    return { kind: "expand", hidden: total - LIST_PREVIEW_COUNT };
  }
  // 已就地展开：只在"还能再露出更多"时才给收起；否则给出弹窗出口。
  if (reveal === "expanded") {
    return total <= LIST_EXPAND_CAP
      ? { kind: "collapse" }
      : { kind: "openPicker", hidden: total - LIST_EXPAND_CAP };
  }
  return { kind: "collapse" };
}
