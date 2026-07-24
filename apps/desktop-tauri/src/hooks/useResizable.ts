import { useState, useCallback, useRef, useEffect } from "react";
import { storage } from "../utils/storageCompat";

export interface UseResizableOptions {
  /** localStorage key (persists width across reloads) */
  storageKey: string;
  /** Fallback width on first render */
  defaultWidth: number;
  /** Lower bound (px) */
  min: number;
  /** Upper bound (px) */
  max: number;
  /**
   * Which edge of the screen the column is anchored to.
   * - "left"  → width = mouse clientX
   * - "right" → width = window.innerWidth - clientX
   */
  side: "left" | "right";
}

export function useResizable({
  storageKey,
  defaultWidth,
  min,
  max,
  side,
}: UseResizableOptions) {
  // ── Hook order (must stay stable across renders; do NOT branch):
  // 1. useRef  (resizingRef)
  // 2. useState (width)
  // 3. useCallback (onMouseDown)
  // 4. useEffect (mousemove/mouseup listeners)
  // 5. useEffect (persist width to storage)

  const resizingRef = useRef(false);

  const [width, setWidth] = useState<number>(() => {
    try {
      const raw = storage.getItem(storageKey);
      const n = raw ? Number(raw) : NaN;
      if (!Number.isFinite(n)) return defaultWidth;
      return Math.min(max, Math.max(min, n));
    } catch {
      return defaultWidth;
    }
  });

  const onMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    resizingRef.current = true;
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, []);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!resizingRef.current) return;
      const candidate = side === "left" ? e.clientX : window.innerWidth - e.clientX;
      setWidth((_prev) => Math.min(max, Math.max(min, candidate)));
    };
    const onUp = () => {
      if (!resizingRef.current) return;
      resizingRef.current = false;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [min, max, side]);

  useEffect(() => {
    try { storage.setItem(storageKey, String(width)); } catch { /* ignore */ }
  }, [storageKey, width]);

  return { width, onMouseDown, setWidth };
}
