// PDF 渲染：pdf.js 直接画到 canvas。
//
// 形态（2026-10-28 改）：**连续纵向滚动**。旧的"一页一张画布 + 上一页/下一页"
// 在长文档里有两个硬伤：看不到自己在第几页（得一页一页点过去），每翻一页还
// 要等一次渲染。现在所有页铺成一列滚动，当前页由滚动位置反推；每页只有接近
// 视口时才画（IntersectionObserver + 一屏预渲染边距），所以 100 页的文档首屏
// 成本与 7 页相同。左侧可开缩略图侧栏，点一下滚到那一页。
//
// 仍是「一页一张 canvas」：pdf.js 的渲染粒度就是页，没有跨页画布这回事。

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import * as pdfjsLib from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { standardFontDataUrl } from "../lib/standardFonts";
import { ChevronLeft, ChevronRight, PanelLeft, RotateCw, ZoomIn, ZoomOut } from "lucide-react";

pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

const MIN_SCALE = 0.5;
const MAX_SCALE = 3;
/** 缩略图渲染宽度（CSS px）：与 .preview-pdf-thumbs 的内容宽对齐，写死免得量两次布局。 */
const THUMB_WIDTH = 104;
/** 视口外多远就开始画：一屏的距离足够让滚动不出现白块。 */
const PRE_RENDER_MARGIN = "800px 0px";

/** 一页在 scale=1、含页面自带 /Rotate 与用户旋转后的尺寸（pt）。 */
interface PageBox {
  w: number;
  h: number;
}

/** 落在纵向位置 y 上的页号（1-based）：tops 是每页在滚动内容里的起点。 */
function pageAtLayout(tops: number[], y: number): number {
  let n = 1;
  for (let i = 0; i < tops.length; i++) {
    if (tops[i] <= y) n = i + 1;
    else break;
  }
  return n;
}

/**
 * 一页画布：尺寸由父级给定（缩放/旋转后），只有进入预渲染范围才真正绘制。
 * 画完的 canvas 不回收——滚回来时重新绘制的闪烁比多占几 MB 更烦。
 */
function PdfPageCanvas({
  doc,
  pageNo,
  width,
  height,
  rotation,
  root,
  elRef,
}: {
  doc: PDFDocumentProxy;
  pageNo: number;
  width: number;
  height: number;
  rotation: number;
  /** 预渲染判定的滚动容器；缩略图侧栏里不传（按视口算）。 */
  root?: HTMLElement | null;
  elRef?: (el: HTMLDivElement | null) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const holderRef = useRef<HTMLDivElement | null>(null);
  const taskRef = useRef<{ cancel: () => void } | null>(null);
  const [near, setNear] = useState(false);

  useEffect(() => {
    const el = holderRef.current;
    if (!el) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) setNear(true);
      },
      { root: root ?? null, rootMargin: PRE_RENDER_MARGIN },
    );
    io.observe(el);
    return () => io.disconnect();
  }, [root]);

  useEffect(() => {
    if (!near || width <= 0 || height <= 0) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    let cancelled = false;
    taskRef.current?.cancel();
    const dpr = window.devicePixelRatio || 1;
    void (async () => {
      try {
        const page = await doc.getPage(pageNo);
        if (cancelled) return;
        // 旋转是绝对角度（替换页面自带 /Rotate），所以基准也按旋转后算——
        // 转 90° 后宽高互换，不换基准会超宽。
        const abs = (page.rotate + rotation) % 360;
        const base = page.getViewport({ scale: 1, rotation: abs });
        const viewport = page.getViewport({ scale: (width / base.width) * dpr, rotation: abs });
        // 高清屏靠 scale×dpr 出像素，CSS 尺寸再定回逻辑像素。
        canvas.width = Math.floor(viewport.width);
        canvas.height = Math.floor(viewport.height);
        canvas.style.width = `${width}px`;
        canvas.style.height = `${height}px`;
        const task = page.render({ canvas, viewport });
        taskRef.current = task;
        await task.promise;
      } catch {
        // 取消渲染（换页 / 缩放 / 卸载）会走到这里，属正常流程
      }
    })();
    return () => {
      cancelled = true;
      taskRef.current?.cancel();
    };
  }, [near, doc, pageNo, width, height, rotation]);

  return (
    <div
      ref={(el) => {
        holderRef.current = el;
        elRef?.(el);
      }}
      className="preview-pdf-sheet"
      style={{ width, height }}
    >
      <canvas ref={canvasRef} />
    </div>
  );
}

export function PdfView({ bytes }: { bytes: Uint8Array }) {
  const [stageEl, setStageEl] = useState<HTMLDivElement | null>(null);
  /** 每页的 DOM 节点：滚动位置与当前页都要靠它们量。 */
  const sheetEls = useRef<Array<HTMLDivElement | null>>([]);
  const thumbEls = useRef<Array<HTMLButtonElement | null>>([]);
  /** 缩放/旋转前的内容锚点：改完按它恢复视口，否则一跳回顶部。 */
  const anchorRef = useRef<{ page: number; frac: number } | null>(null);
  const thumbsInit = useRef(false);

  const [doc, setDoc] = useState<PDFDocumentProxy | null>(null);
  const [boxes, setBoxes] = useState<PageBox[]>([]);
  /** 每页在滚动内容里的纵向起点，缩放/旋转/换文档后重算。 */
  const [tops, setTops] = useState<number[]>([]);
  /** 可用于排版的宽度（滚动容器宽 − 左右内边距）。 */
  const [stageWidth, setStageWidth] = useState(0);
  const [scale, setScale] = useState(1);
  const [rotation, setRotation] = useState(0);
  const [currentPage, setCurrentPage] = useState(1);
  const [thumbsOpen, setThumbsOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // 打开文档。bytes 每次都是新引用，副本必须在这里做——pdf.js 会转移
  // ArrayBuffer 的所有权，直接传原始 bytes 会让它后续变成 detached。
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setDoc(null);
    setBoxes([]);
    setTops([]);
    setCurrentPage(1);
    const copy = new Uint8Array(bytes).buffer;
    // 持有 loadingTask 而不是只拿 doc：销毁入口在 task 上，它同时会结束 worker。
    const loadingTask = pdfjsLib.getDocument({
      data: copy,
      // 标准 14 号字的度量：不给的话字宽算不出来（文本层点不中）、
      // 标准字体的渲染也会走回退字体。
      ...(standardFontDataUrl ? { standardFontDataUrl } : {}),
    });
    loadingTask.promise
      .then((d) => {
        if (!cancelled) setDoc(d);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(`PDF 解析失败：${String(e)}`);
      });
    return () => {
      cancelled = true;
      void loadingTask.destroy();
    };
  }, [bytes]);

  // 各页原始尺寸。铺版要先把高度算出来（占位页要有确定高度，否则滚动条
  // 会随渲染一截一截长）。取的是页面字典，不解码内容，代价很小。
  useEffect(() => {
    if (!doc) return;
    let cancelled = false;
    void (async () => {
      const out: PageBox[] = [];
      for (let n = 1; n <= doc.numPages; n++) {
        if (cancelled) return;
        try {
          const page = await doc.getPage(n);
          const vp = page.getViewport({ scale: 1, rotation: (page.rotate + rotation) % 360 });
          out.push({ w: vp.width, h: vp.height });
        } catch {
          out.push({ w: 595, h: 842 }); // 单页取不到时不至于让整份文档塌掉
        }
      }
      if (!cancelled) setBoxes(out);
    })();
    return () => {
      cancelled = true;
    };
  }, [doc, rotation]);

  // 容器宽度（响应式：桌面右栏收窄、全屏铺开都走这里）。
  useEffect(() => {
    if (!stageEl) return;
    const sync = () => setStageWidth(stageEl.clientWidth - 32); // 左右各 16 内边距
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(stageEl);
    return () => ro.disconnect();
  }, [stageEl]);

  const pageCount = doc?.numPages ?? 0;
  const maxW = boxes.reduce((m, b) => Math.max(m, b.w), 0);
  /** scale=1 时"适应宽度"的比例：先按容器定基准，再乘用户缩放。 */
  const fit = maxW > 0 && stageWidth > 0 ? stageWidth / maxW : 0;

  // 铺版后量出每页的纵向起点（滚动定位、当前页判定都基于它）。
  useLayoutEffect(() => {
    if (!stageEl || boxes.length === 0 || fit <= 0) {
      setTops([]);
      return;
    }
    const stageTop = stageEl.getBoundingClientRect().top;
    const scrolled = stageEl.scrollTop;
    setTops(
      boxes.map((_, i) => {
        const el = sheetEls.current[i];
        return el ? el.getBoundingClientRect().top - stageTop + scrolled : 0;
      }),
    );
  }, [boxes, fit, scale, rotation, stageEl]);

  // 滚动 → 反推当前页。取视口 40% 高度处：一页刚好占满视口时不会来回跳。
  useEffect(() => {
    if (!stageEl || tops.length === 0) return;
    let raf = 0;
    const sync = () => {
      raf = 0;
      const y = stageEl.scrollTop + stageEl.clientHeight * 0.4;
      const n = pageAtLayout(tops, y);
      setCurrentPage((prev) => (prev === n ? prev : n));
    };
    const onScroll = () => {
      if (!raf) raf = requestAnimationFrame(sync);
    };
    stageEl.addEventListener("scroll", onScroll, { passive: true });
    sync();
    return () => {
      stageEl.removeEventListener("scroll", onScroll);
      if (raf) cancelAnimationFrame(raf);
    };
  }, [stageEl, tops]);

  // 缩略图侧栏默认值：宽面板（全屏 / 独立查看器）给，窄栏（桌面右栏）不给
  // ——右栏里侧栏会吃掉正文一半宽。只判一次，之后由用户自己开关。
  useEffect(() => {
    if (thumbsInit.current || stageWidth === 0) return;
    thumbsInit.current = true;
    if (stageWidth >= 520) setThumbsOpen(true);
  }, [stageWidth]);

  // 当前页的缩略图跟着滚进可视区（block:"nearest"：已经看得见就不动）。
  useEffect(() => {
    if (!thumbsOpen) return;
    thumbEls.current[currentPage - 1]?.scrollIntoView({ block: "nearest" });
  }, [currentPage, thumbsOpen]);

  const scrollToPage = useCallback(
    (n: number) => {
      if (!stageEl || tops.length === 0) return;
      const target = Math.min(Math.max(n, 1), tops.length);
      stageEl.scrollTo({ top: Math.max(0, tops[target - 1] - 8), behavior: "smooth" });
      setCurrentPage(target);
    },
    [stageEl, tops],
  );

  /** 记下"视口顶部那一页 + 页内相对位置"，缩放/旋转后按它还原。 */
  const captureAnchor = useCallback(() => {
    if (!stageEl || tops.length === 0) return;
    const page = pageAtLayout(tops, stageEl.scrollTop + 1);
    const el = sheetEls.current[page - 1];
    anchorRef.current = {
      page,
      frac: (stageEl.scrollTop - tops[page - 1]) / Math.max(el?.offsetHeight ?? 1, 1),
    };
  }, [stageEl, tops]);

  const zoomStep = useCallback(
    (dir: 1 | -1) => {
      const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, +(scale + dir * 0.25).toFixed(2)));
      // 已到上下限就直接返回：别留一个永不消费的锚点，否则下一次重排
      // （改窗口宽度）会拿旧锚点把滚动位置拽回去。
      if (next === scale) return;
      captureAnchor();
      setScale(next);
    },
    [scale, captureAnchor],
  );

  const rotate = useCallback(() => {
    captureAnchor();
    setRotation((r) => (r + 90) % 360);
  }, [captureAnchor]);

  // 铺版/缩放/旋转落定后按锚点恢复视口（首帧没有锚点，跳过）。
  useLayoutEffect(() => {
    const a = anchorRef.current;
    if (!a || !stageEl || tops.length === 0) return;
    anchorRef.current = null;
    const el = sheetEls.current[a.page - 1];
    stageEl.scrollTop = Math.max(0, tops[a.page - 1] + a.frac * (el?.offsetHeight ?? 0));
  }, [tops, stageEl]);

  // Ctrl+滚轮缩放：React 的 wheel 委托是 passive 的，preventDefault 无效，
  // 必须原生监听 { passive: false } 才能拦下浏览器的页面缩放。
  useEffect(() => {
    if (!stageEl) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      zoomStep(e.deltaY < 0 ? 1 : -1);
    };
    stageEl.addEventListener("wheel", onWheel, { passive: false });
    return () => stageEl.removeEventListener("wheel", onWheel);
  }, [stageEl, zoomStep]);

  // 键盘快捷缩放：挂 window 而不是画布——画布上没有焦点概念，焦点在
  // 文件列表里时也该生效。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        zoomStep(1);
      } else if (e.key === "-") {
        e.preventDefault();
        zoomStep(-1);
      } else if (e.key === "0") {
        e.preventDefault();
        if (scale === 1 && rotation === 0) return;
        captureAnchor();
        setScale(1);
        setRotation(0);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [zoomStep, captureAnchor, scale, rotation]);

  return (
    <div className="preview-pdf">
      <div className="preview-pdf-bar">
        <button
          type="button"
          className="head-action"
          disabled={currentPage <= 1}
          onClick={() => scrollToPage(currentPage - 1)}
          aria-label="上一页"
          title="上一页"
        >
          <ChevronLeft className="icon-md" />
        </button>
        <span className="preview-pdf-page">
          {pageCount ? `${currentPage} / ${pageCount}` : "载入中…"}
        </span>
        <button
          type="button"
          className="head-action"
          disabled={pageCount === 0 || currentPage >= pageCount}
          onClick={() => scrollToPage(currentPage + 1)}
          aria-label="下一页"
          title="下一页"
        >
          <ChevronRight className="icon-md" />
        </button>
        <button
          type="button"
          className={thumbsOpen ? "head-action is-active" : "head-action"}
          onClick={() => setThumbsOpen((v) => !v)}
          aria-pressed={thumbsOpen}
          aria-label={thumbsOpen ? "隐藏页面缩略图" : "显示页面缩略图"}
          title={thumbsOpen ? "隐藏页面缩略图" : "显示页面缩略图（点缩略图跳页）"}
        >
          <PanelLeft className="icon-md" />
        </button>
        <span className="preview-pdf-spacer" />
        <button
          type="button"
          className="head-action"
          onClick={() => zoomStep(-1)}
          aria-label="缩小（Ctrl+- 或 Ctrl+滚轮）"
          title="缩小（Ctrl+- 或 Ctrl+滚轮）"
        >
          <ZoomOut className="icon-md" />
        </button>
        <span className="preview-pdf-zoom">{Math.round(scale * 100)}%</span>
        <button
          type="button"
          className="head-action"
          onClick={() => zoomStep(1)}
          aria-label="放大（Ctrl+= 或 Ctrl+滚轮）"
          title="放大（Ctrl+= 或 Ctrl+滚轮）"
        >
          <ZoomIn className="icon-md" />
        </button>
        <button
          type="button"
          className="head-action"
          onClick={rotate}
          aria-label={`旋转 90°（当前 ${rotation}°）`}
          title="顺时针旋转 90°（Ctrl+0 复位）"
        >
          <RotateCw className="icon-md" />
        </button>
      </div>

      <div className="preview-pdf-body">
        {thumbsOpen && doc && boxes.length > 0 && (
          <div className="preview-pdf-thumbs" role="group" aria-label="页面缩略图">
            {boxes.map((b, i) => {
              const n = i + 1;
              return (
                <button
                  key={n}
                  ref={(el) => {
                    thumbEls.current[i] = el;
                  }}
                  type="button"
                  className={n === currentPage ? "preview-pdf-thumb is-active" : "preview-pdf-thumb"}
                  onClick={() => scrollToPage(n)}
                  aria-label={`跳到第 ${n} 页`}
                  aria-current={n === currentPage ? "page" : undefined}
                >
                  <PdfPageCanvas
                    doc={doc}
                    pageNo={n}
                    width={THUMB_WIDTH}
                    height={Math.round(THUMB_WIDTH * (b.h / b.w))}
                    rotation={rotation}
                  />
                  <span className="preview-pdf-thumb-no">{n}</span>
                </button>
              );
            })}
          </div>
        )}

        <div className="preview-pdf-stage" ref={setStageEl}>
          {error ? (
            <div className="preview-state-error">{error}</div>
          ) : doc && boxes.length > 0 && fit > 0 ? (
            <div className="preview-pdf-pages">
              {boxes.map((b, i) => (
                <PdfPageCanvas
                  key={i + 1}
                  doc={doc}
                  pageNo={i + 1}
                  width={Math.round(b.w * fit * scale)}
                  height={Math.round(b.h * fit * scale)}
                  rotation={rotation}
                  root={stageEl}
                  elRef={(el) => {
                    sheetEls.current[i] = el;
                  }}
                />
              ))}
            </div>
          ) : (
            <div className="preview-state-center">正在载入页面…</div>
          )}
        </div>
      </div>
    </div>
  );
}
