import { useEffect, type RefObject } from "react";

/**
 * Closes a popover/menu when a pointerdown or touchstart happens outside the
 * referenced element. Ignores events whose target is inside `ignoreRefs` so
 * a trigger button can also be excluded from the "outside" set.
 *
 * Only listens while `active` is true so we don't pay the global cost when
 * nothing is open.
 *
 * We skip the single pointerdown that immediately follows an open flip:
 * a parent re-render (e.g. selecting an expert) can re-attach the trigger
 * ref to a fresh DOM node in the same frame, so the opening gesture must not
 * be treated as an outside click. Real outside clicks arrive at least a
 * frame later, so the skip is invisible to the user.
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
