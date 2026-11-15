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
  /**
   * 历史默认值清单：存储值命中其一即视为「没拖过、用旧默认」，本次直接
   * 采用 defaultWidth（持久化 effect 会随即把新默认写回）。
   * 迁移必须发生在 state 初始化时 —— 若在 effect 里改 storage，本 hook
   * 的持久化 effect 会把旧 state 原样写回，迁移被原地覆盖（2026-10-15 踩过）。
   */
  legacyDefaults?: string[];
}

export function useResizable({
  storageKey,
  defaultWidth,
  min,
  max,
  side,
  legacyDefaults,
}: UseResizableOptions) {
  // Hook order must stay stable across renders — never branch around
  // these calls.

  const resizingRef = useRef(false);

  const [width, setWidth] = useState<number>(() => {
    try {
      const raw = storage.getItem(storageKey);
      if (raw && legacyDefaults?.includes(raw)) return defaultWidth;
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
