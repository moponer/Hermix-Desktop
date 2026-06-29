import { useLayoutEffect, useState, type ReactNode } from "react";

/**
 * The popovers in ChatView (expert / workspace / model / permission) all
 * share the same look: a panel floating above the trigger, fixed
 * rounded-xl, z-50, with an optional caption header and a body region
 * that scrolls if it overflows. Before this component existed each
 * popover re-implemented the same `<div className="absolute bottom-full
 * mb-2 left-0 w-72 rounded-xl bg-bg-elevated shadow-2xl z-50
 * overflow-hidden">` boilerplate and they had subtly different border
 * treatment and inconsistent z-index, which is exactly the kind of
 * drift that bites later.
 *
 * The shell now owns:
 *   - positioning (`align: "left" | "right"`, `side: "top" | "bottom"`)
 *   - width (`w-56` / `w-72` / `w-80` preset)
 *   - the rounded/elevated/shadow look
 *   - z-index (`z-50`)
 *   - consistent 1px border on light theme for definition
 *
 * v22 update: switched from `position: absolute` (anchored to the
 * nearest positioned ancestor — which inside ChatView's `overflow:
 * hidden` chain meant the popover could be clipped by a scroll
 * container on the message list) to `position: fixed` anchored to
 * the trigger's bounding rect. The popover now lives in the viewport
 * and is no longer subject to ancestor overflow.
 *
 * Body content is laid out by the consumer; sections inside the body
 * should use the `<PopoverHeader>` / `<PopoverFooter>` helpers so the
 * caption row stays visually identical across popovers.
 */
export type PopoverAlign = "left" | "center" | "right";
export type PopoverSide = "top" | "bottom";
export type PopoverWidth = "sm" | "md" | "lg"; // 52 / 64 / 72 (Tailwind)

const WIDTH_PX: Record<PopoverWidth, number> = {
  sm: 176, // w-44
  md: 224, // w-56
  lg: 256, // w-64
};

const SIDE_GAP_PX = 6; // small gap between trigger and popover

export interface PopoverShellProps {
  /** MUST be attached to the popover root — used for click-outside & ESC. */
  containerRef: React.RefObject<HTMLDivElement | null>;
  /**
   * The button that opened this popover. Used to anchor the popover via
   * `getBoundingClientRect()` so it doesn't get clipped by ancestor
   * `overflow: hidden` (a long-standing bug on the model/permission
   * popovers in narrow viewports). If absent, falls back to legacy
   * `position: absolute` anchoring.
   */
  triggerRef?: React.RefObject<HTMLElement | null>;
  children: ReactNode;
  align?: PopoverAlign;
  side?: PopoverSide;
  width?: PopoverWidth;
  /** Extra classes appended to the shell (e.g. `max-h-[80vh]`). */
  className?: string;
}

export function PopoverShell({
  containerRef,
  triggerRef,
  children,
  align = "center",
  side = "top",
  width = "md",
  className = "",
}: PopoverShellProps) {
  // When we have a trigger, anchor to its viewport position via
  // `position: fixed` (immune to ancestor overflow). When we don't,
  // fall back to the legacy `position: absolute` placement.
  const [pos, setPos] = useState<{ top: number; left: number } | null>(null);

  useLayoutEffect(() => {
    if (!triggerRef?.current) return;
    const compute = () => {
      if (!triggerRef.current) return;
      const r = triggerRef.current.getBoundingClientRect();
      const popWidth = WIDTH_PX[width];
      let left: number;
      if (align === "right") left = r.right - popWidth;
      else if (align === "center") left = r.left + r.width / 2 - popWidth / 2;
      else left = r.left;
      const top = side === "top" ? r.top - SIDE_GAP_PX : r.bottom + SIDE_GAP_PX;
      setPos({ top, left });
    };
    compute();
    window.addEventListener("resize", compute);
    window.addEventListener("scroll", compute, true);
    return () => {
      window.removeEventListener("resize", compute);
      window.removeEventListener("scroll", compute, true);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [triggerRef, align, side, width]);

  const anchorStyle = pos
    ? ({
        position: "fixed" as const,
        top: pos.top,
        left: pos.left,
        transform: side === "top" ? "translateY(-100%)" : undefined,
      } as React.CSSProperties)
    : undefined;

  // Width class lives OUTSIDE the `legacyClass` ternary so it survives
  // the pos-null → pos-set transition (otherwise the popover collapses
  // to content width the moment useLayoutEffect sets the anchor rect,
  // making the panel look like a tiny sliver).
  const widthClass = width === "sm" ? "w-44" : width === "lg" ? "w-64" : "w-56";

  // Legacy class set used only when `triggerRef` isn't provided (i.e.
  // pos is null). When pos is set, we use `position: fixed` anchored to
  // the viewport via style, so no absolute positioning is needed.
  const legacyClass = !pos
    ? `absolute ${side === "top" ? "bottom-full mb-0" : "top-full mt-0"} ${
        align === "left" ? "left-0" : align === "right" ? "right-0" : "left-1/2 -translate-x-1/2"
      }`
    : "";

  return (
    <div
      ref={containerRef}
      role="dialog"
      aria-modal="false"
      style={anchorStyle}
      onPointerDown={(e) => e.stopPropagation()}
      className={`${widthClass} shrink-0 rounded-[10px] bg-bg-elevated border border-border-primary shadow-pop p-1.5 overflow-hidden z-[60] ${legacyClass} ${className}`}
    >
      {children}
    </div>
  );
}

/**
 * Caption row used at the top of every popover. Lowercase, 11px / 600 /
 * ink-soft — matches the v2 popover detail spec (12.5px font-size
 * items, 6/12/4px header padding) and is consistent across the four
 * ChatView popovers (expert / workspace / model / permission).
 */
export function PopoverHeader({
  children,
  right,
  className = "",
}: {
  children: ReactNode;
  right?: ReactNode;
  className?: string;
}) {
  return (
    <div className={`px-2.5 pt-1 pb-1 flex items-center justify-between ${className}`}>
      <span className="text-[10.5px] font-semibold text-text-faint">
        {children}
      </span>
      {right}
    </div>
  );
}

/**
 * Body region that scrolls when its content overflows. Use for
 * scrollable lists. Without this, long lists grow past the popover
 * and clip into the chat area below.
 */
export function PopoverBody({
  children,
  maxHeight = "max-h-72",
  className = "",
}: {
  children: ReactNode;
  /** Tailwind max-h-* class. Defaults to 18rem. */
  maxHeight?: string;
  className?: string;
}) {
  return (
    <div className={`${maxHeight} overflow-y-auto ${className}`}>
      {children}
    </div>
  );
}

/**
 * Bottom strip (e.g. "不绑定目录" escape hatch on the workspace
 * popover). Stays visually distinct from the body.
 */
export function PopoverFooter({
  children,
  className = "",
}: {
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={`border-t border-border-primary ${className}`}>
      {children}
    </div>
  );
}
