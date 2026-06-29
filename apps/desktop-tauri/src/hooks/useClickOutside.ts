import { useEffect, type RefObject } from "react";

/**
 * Closes a popover/menu when a pointerdown or touchstart happens outside the
 * referenced element. Ignores events whose target is inside `ignoreRefs` so
 * a trigger button can also be excluded from the "outside" set.
 *
 * Only listens while `active` is true so we don't pay the global cost when
 * nothing is open.
 *
 * 2026-06-25 主人反馈 "对话框的专家，每次都选择好几次好才能显示"：
 *   根因是 pointerdown 在 click 之前触发。当 open 翻为 true：
 *     - 旧版 useEffect 在 active=true 时立刻注册 listener
 *     - 这一次注册的 listener 不会处理"导致开的那次 pointerdown"
 *       （它已经过去了），所以开是正常的
 *   真正的 bug 路径：用户开 → 选一个专家 → setExpert + setOpen(false)
 *   → setOpen(false) 让 active 翻 false → effect 卸载 listener。
 *   但同时因为 storage.setItem 是同步的，状态更新也是同步的，
 *   所以 useSessionExpert 的 expert 状态会立即更新，触发 ChatView
 *   重渲染，toolbar 重渲染，整个 row 上的所有 popover 重建，trigger
 *   ref 被重新 attach 到一个新 DOM 节点。
 *
 *   修法：把 "开" 的那一帧视为 "skip"，因为那一帧可能是组件重建
 *   的过渡阶段（特别是父级重渲染导致 ref 重建）。rAF 之后恢复正
 *   常逻辑。这只跳过紧跟 open 翻转的 1 个 pointerdown 事件，完
 *   全不影响正常 "点击外部关闭" 行为（用户的"外部点击"会在开之
 *   后至少 1 帧才来）。
 */
export function useClickOutside(
  ref: RefObject<HTMLElement | null>,
  onOutside: () => void,
  active = true,
  ignoreRefs: RefObject<HTMLElement | null>[] = [],
) {
  useEffect(() => {
    if (!active) return;

    let skipNext = true;
    const cancelSkip = requestAnimationFrame(() => {
      skipNext = false;
    });

    const handler = (e: PointerEvent | TouchEvent) => {
      if (skipNext) {
        skipNext = false;
        return;
      }
      const target = e.target as Node | null;
      if (!target) return;
      if (ref.current && ref.current.contains(target)) return;
      for (const r of ignoreRefs) {
        if (r.current && r.current.contains(target)) return;
      }
      onOutside();
    };

    document.addEventListener("pointerdown", handler);
    return () => {
      document.removeEventListener("pointerdown", handler);
      cancelAnimationFrame(cancelSkip);
    };
  }, [ref, onOutside, active, ignoreRefs]);
}
