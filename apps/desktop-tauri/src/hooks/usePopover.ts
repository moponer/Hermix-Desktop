import { useCallback, useRef, useState } from "react";
import { useClickOutside } from "./useClickOutside";
import { useEscClose } from "./useEscClose";

/**
 * Drop-in state for a popover: `[open, setOpen, containerRef, triggerRef, toggle, close]`.
 * - `containerRef` MUST be attached to the popover root element
 *   (consumers should pass it to <PopoverShell containerRef={...} />).
 * - `triggerRef` SHOULD be attached to the button that toggles it,
 *   so clicking the trigger doesn't immediately close what it just opened.
 * - Clicking outside the popover (and not on the trigger) closes it.
 * - Pressing Escape closes it.
 */
export function usePopover() {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLElement | null>(null);

  const close = useCallback(() => setOpen(false), []);
  const toggle = useCallback(() => setOpen((v) => !v), []);

  useClickOutside(containerRef, close, open, [triggerRef]);
  useEscClose(close, open);

  return [open, setOpen, containerRef, triggerRef, toggle, close] as const;
}
