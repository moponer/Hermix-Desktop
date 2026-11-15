import { useEffect, type RefObject } from "react";

const FOCUSABLE =
  'button, [href], input, select, textarea, details, summary, [contenteditable="true"], [tabindex]:not([tabindex="-1"])';

/**
 * 焦点陷阱：Tab / Shift+Tab 在容器内循环，打开时把焦点移进去，关闭时还回来。
 *
 * 凡是声明了 `role="dialog"` + `aria-modal="true"` 的容器都该配它 ——
 * 否则读屏用户按一下 Tab，焦点就跑到遮罩后面的内容上，「模态」形同虚设。
 *
 * 之前这段逻辑内联在 `ui/Modal` 里，只有模态框用得上；
 * `PreviewPanel` 的全屏覆盖层同样声明了 aria-modal="true"，却没有陷阱
 * （审计问题 21 的最后一块）。抽成 hook 后两处共用。
 *
 * @param ref 容器 ref
 * @param active 是否激活（关着时不做任何事）
 * @param autoFocusSelector 初始落焦元素的选择器；缺省取第一个可聚焦控件
 */
export function useFocusTrap(
  ref: RefObject<HTMLElement | null>,
  active: boolean,
  autoFocusSelector?: string,
): void {
  useEffect(() => {
    if (!active) return;
    const panel = ref.current;
    if (!panel) return;
    // 记住打开前的焦点，关闭时还回去。
    const restore = document.activeElement as HTMLElement | null;

    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Tab") return;
      const focusables = Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
        (el) => !el.hasAttribute("disabled") && el.offsetParent !== null,
      );
      if (focusables.length === 0) return;
      const first = focusables[0]!;
      const last = focusables[focusables.length - 1]!;
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", onKey, true);
    const t = window.setTimeout(() => {
      const preferred = autoFocusSelector
        ? panel.querySelector<HTMLElement>(autoFocusSelector)
        : null;
      (preferred ?? panel.querySelector<HTMLElement>("button, input"))?.focus();
    }, 0);

    return () => {
      document.removeEventListener("keydown", onKey, true);
      window.clearTimeout(t);
      restore?.focus?.();
    };
  }, [ref, active, autoFocusSelector]);
}
