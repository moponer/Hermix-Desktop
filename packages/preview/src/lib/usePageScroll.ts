// 连续滚动文档的"第几页"，三个渲染器（PDF / PPTX / Word）共用这一份实现：
//   1) 滚动位置 → 当前页号（取视口 40% 高度处，一页刚好铺满时不会来回跳）；
//   2) 点缩略图 → 滚到那一页；
//   3) 缩放/旋转前后的视口锚点（不记的话改一下缩放就跳回顶部）。
//
// 这里不关心一页是怎么画出来的：页元素由渲染器注册进来，量的是真实 DOM 矩形，
// 所以 canvas 页（PDF/PPTX）与 DOM 页（Word 的 section.docx）走同一条路径。

import { useCallback, useEffect, useRef, useState } from "react";
import type { RefObject } from "react";

export interface PageAnchor {
  /** 1-based 页号。 */
  page: number;
  /** 视口顶部落在该页的哪个相对位置（0~1）。 */
  frac: number;
}

export interface PageScroll {
  /** 当前页（1-based）。 */
  current: number;
  /** 注册第 i 页（0-based）的 DOM 节点，直接当 ref 用。 */
  register: (i: number) => (el: HTMLElement | null) => void;
  /** 已注册的节点（惰性渲染要在外面 observe 它们）。 */
  els: RefObject<Array<HTMLElement | null>>;
  scrollTo: (page: number) => void;
  capture: () => PageAnchor | null;
  restore: (anchor: PageAnchor | null) => void;
}

/** 元素在滚动内容坐标系里的纵向起点（含当前滚动量）。 */
function contentTop(el: HTMLElement, stage: HTMLElement): number {
  return el.getBoundingClientRect().top - stage.getBoundingClientRect().top + stage.scrollTop;
}

export function usePageScroll(stage: HTMLElement | null, count: number): PageScroll {
  const els = useRef<Array<HTMLElement | null>>([]);
  const stageRef = useRef<HTMLElement | null>(null);
  // 直接赋值而不是 effect 同步：回调里读到的永远是最新容器。
  stageRef.current = stage;
  const [current, setCurrent] = useState(1);

  const indexAt = useCallback(
    (y: number) => {
      const s = stageRef.current;
      if (!s) return 1;
      let n = 1;
      // 只认"这一份文档的、还挂在 DOM 上"的页：换文档后旧节点会残留，
      // 它们的矩形全在 0,0，混进来会把页码算成两位数。
      const end = Math.min(count, els.current.length);
      for (let i = 0; i < end; i++) {
        const el = els.current[i];
        if (!el || !el.isConnected) continue;
        if (contentTop(el, s) <= y) n = i + 1;
        else break;
      }
      return n;
    },
    [count],
  );

  useEffect(() => {
    if (!stage) return;
    let raf = 0;
    const sync = () => {
      raf = 0;
      const n = indexAt(stage.scrollTop + stage.clientHeight * 0.4);
      setCurrent((prev) => (prev === n ? prev : n));
    };
    const onScroll = () => {
      if (!raf) raf = requestAnimationFrame(sync);
    };
    stage.addEventListener("scroll", onScroll, { passive: true });
    sync();
    // count 进依赖：页数变了（换文档）要重新量一遍。
    return () => {
      stage.removeEventListener("scroll", onScroll);
      if (raf) cancelAnimationFrame(raf);
    };
  }, [stage, count, indexAt]);

  const register = useCallback(
    (i: number) => (el: HTMLElement | null) => {
      els.current[i] = el;
    },
    [],
  );

  const scrollTo = useCallback(
    (page: number) => {
      const s = stageRef.current;
      const target = Math.min(Math.max(page, 1), Math.max(count, 1));
      const el = els.current[target - 1];
      if (!s || !el) return;
      s.scrollTo({ top: Math.max(0, contentTop(el, s) - 8), behavior: "smooth" });
      setCurrent(target);
    },
    [count],
  );

  const capture = useCallback((): PageAnchor | null => {
    const s = stageRef.current;
    if (!s) return null;
    const page = indexAt(s.scrollTop + 1);
    const el = els.current[page - 1];
    if (!el) return null;
    return { page, frac: (s.scrollTop - contentTop(el, s)) / Math.max(el.offsetHeight, 1) };
  }, [indexAt]);

  const restore = useCallback((anchor: PageAnchor | null) => {
    const s = stageRef.current;
    if (!s || !anchor) return;
    const el = els.current[anchor.page - 1];
    if (!el) return;
    s.scrollTop = Math.max(0, contentTop(el, s) + anchor.frac * el.offsetHeight);
  }, []);

  return { current, register, els, scrollTo, capture, restore };
}
