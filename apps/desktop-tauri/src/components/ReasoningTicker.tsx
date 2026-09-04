// ReasoningTicker — single-line ticker that shows the LATEST captured
// reasoning content from the active assistant turn. Mount gating lives
// in the parent: it only mounts when the reasoning fold is REALLY
// closed (reasoningFolded=true, initial state false in ChatView).
//
// 2026-08-29 rework (user feedback):
//  1. Always render the LATEST line. The old version cycled from
//     index 0 on a 3s interval, but streaming rebuilds `lines` every
//     chunk and the reset-to-0 effect kept snapping the index back to
//     0 — users only ever saw line 1 and occasionally line 2. Stale
//     content is noise: show the newest reasoning or nothing.
//  2. Fast marquee — when the latest line overflows the pill it
//     scrolls horizontally on a fast loop (CSS) instead of ellipsing
//     into a static snapshot. Short lines render statically.
//  3. During a live stream the thinking block is always expanded and
//     visible inline, so the parent keeps this component unmounted —
//     no duplicate rolling copy next to readable reasoning.
import { useEffect, useRef, useState } from "react";

export interface ReasoningTickerProps {
  /** Captured reasoning lines from the active assistant turn
   *  (chronological order). Only the LAST entry is displayed.
   *  Empty array → renders nothing. */
  lines: string[];
}

/** Overflow threshold: the pill fits ~26 CJK chars at 12px inside its
 *  320px max-width. Past this we switch to the marquee loop. */
const MARQUEE_THRESHOLD = 26;

export function ReasoningTicker({ lines }: ReasoningTickerProps) {
  // Defensive: filter out blanks (collapsed-thinking edits can leave
  // empty `reasoning` steps) and short-circuit on empty input.
  const filtered = lines.filter(
    (l) => typeof l === "string" && l.trim().length > 0,
  );
  const latest = filtered[filtered.length - 1];

  // Track overflow with a ref probe so short lines don't marquee.
  const spanRef = useRef<HTMLSpanElement>(null);
  const [overflows, setOverflows] = useState(false);

  useEffect(() => {
    const el = spanRef.current;
    if (!el) return;
    setOverflows(el.scrollWidth > el.clientWidth + 1);
  }, [latest]);

  if (!latest) return null;
  const marquee = overflows || latest.length > MARQUEE_THRESHOLD;
  return (
    <div className="reasoning-ticker" title={latest}>
      <span
        ref={spanRef}
        className={`reasoning-ticker-line${marquee ? " reasoning-ticker-marquee" : ""}`}
      >
        {latest}
      </span>
    </div>
  );
}
