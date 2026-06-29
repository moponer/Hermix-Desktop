import { useEffect } from "react";

/**
 * Closes a popover/menu when the user presses Escape while it is open.
 * - `active` controls whether the listener is attached. Pass the same
 *   `open` boolean you use to render the popover, so we don't pay the
 *   global cost when nothing is open.
 * - Uses the capture phase so a popover nested inside another popover
 *   still closes when the user hits ESC. The listener self-removes
 *   on cleanup.
 */
export function useEscClose(onClose: () => void, active = true) {
  useEffect(() => {
    if (!active) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      onClose();
    };
    document.addEventListener("keydown", handler, true);
    return () => document.removeEventListener("keydown", handler, true);
  }, [onClose, active]);
}
