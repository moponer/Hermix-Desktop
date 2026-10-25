// PDF 渲染：pdf.js 直接画到 canvas。
//
// 之前用 `<embed type="application/pdf">`，等于把渲染交给 WebView 内置
// 查看器——没有页码、没有缩放、没法选中文本，且在不同机器上表现不一致
// （有的 WebView2 根本不渲染）。换成 pdf.js 后这些都在我们自己手里。

import { useCallback, useEffect, useRef, useState } from "react";
import * as pdfjsLib from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { standardFontDataUrl } from "../lib/standardFonts";
import { ChevronLeft, ChevronRight, RotateCw, ZoomIn, ZoomOut } from "lucide-react";

pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

const MIN_SCALE = 0.5;
const MAX_SCALE = 3;

export function PdfView({ bytes }: { bytes: Uint8Array }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const docRef = useRef<PDFDocumentProxy | null>(null);
  const taskRef = useRef<ReturnType<typeof pdfjsLib.getDocument> | null>(null);
  const renderTaskRef = useRef<{ cancel: () => void } | null>(null);

  const [pageCount, setPageCount] = useState(0);
  const [pageNo, setPageNo] = useState(1);
  const [scale, setScale] = useState(1);
  /** 相对页面自带 /Rotate 的附加旋转（90° 步进）。 */
  const [rotation, setRotation] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [rendering, setRendering] = useState(false);
  /** 缩放前滚动容器中心的内容比例锚点：渲染完成后按它恢复视口，
   *  否则放大后直接跳回顶部，读长文档没法用快捷键连续放大。 */
  const anchorRef = useRef<{ fx: number; fy: number } | null>(null);

  // 打开文档。bytes 每次都是新引用，副本必须在这里做——pdf.js 会转移
  // ArrayBuffer 的所有权，直接传原始 bytes 会让它后续变成 detached。
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setPageCount(0);
    const copy = new Uint8Array(bytes).buffer;
    // 持有 loadingTask 而不是只拿 doc：销毁入口在 task 上，它同时会
    // 结束 worker。只丢 doc 引用会让解析线程留着直到 GC。
    const loadingTask = pdfjsLib.getDocument({
      data: copy,
      // 标准 14 号字的度量：不给的话字宽算不出来（文本层点不中）、
      // 标准字体的渲染也会走回退字体。
      ...(standardFontDataUrl ? { standardFontDataUrl } : {}),
    });
    taskRef.current = loadingTask;
    loadingTask.promise
      .then((doc) => {
        if (cancelled) return;
        docRef.current = doc;
        setPageCount(doc.numPages);
        setPageNo(1);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(`PDF 解析失败：${String(e)}`);
      });
    return () => {
      cancelled = true;
      void taskRef.current?.destroy();
      taskRef.current = null;
      docRef.current = null;
    };
  }, [bytes]);

  const render = useCallback(async () => {
    const doc = docRef.current;
    const canvas = canvasRef.current;
    if (!doc || !canvas) return;

    renderTaskRef.current?.cancel();
    setRendering(true);
    try {
      const page = await doc.getPage(pageNo);
      // rotation 是绝对角度（替换页面自带 /Rotate），叠加自转量要加上它；
      // fit 基准也按旋转后的宽算——转 90° 后宽高互换，不换基准会超宽。
      const absRotation = (page.rotate + rotation) % 360;
      const base = page.getViewport({ scale: 1, rotation: absRotation });
      // 先按容器宽度定基准，再乘缩放——这样 scale=1 就是"适应宽度"
      const fit = canvas.parentElement
        ? (canvas.parentElement.clientWidth - 32) / base.width
        : 1;
      const dpr = window.devicePixelRatio || 1;
      const viewport = page.getViewport({ scale: fit * scale * dpr, rotation: absRotation });

      // pdf.js v6 直接吃 canvas 元素；高清屏通过 scale 乘 dpr 解决，
      // CSS 尺寸再除回去，显示大小与逻辑像素一致。
      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      canvas.style.width = `${Math.floor(viewport.width / dpr)}px`;
      canvas.style.height = `${Math.floor(viewport.height / dpr)}px`;

      const task = page.render({ canvas, viewport });
      renderTaskRef.current = task;
      await task.promise;
    } catch {
      // 取消渲染会走到这里，属正常流程
    } finally {
      setRendering(false);
    }
  }, [pageNo, scale, rotation]);

  /** 步进缩放（±0.25，clamp 到上下限），并记下滚动锚点等渲染后恢复。 */
  const zoomStep = useCallback((dir: 1 | -1) => {
    const stage = stageRef.current;
    if (stage) {
      anchorRef.current = {
        fx: (stage.scrollLeft + stage.clientWidth / 2) / Math.max(stage.scrollWidth, 1),
        fy: (stage.scrollTop + stage.clientHeight / 2) / Math.max(stage.scrollHeight, 1),
      };
    }
    setScale((s) => Math.min(MAX_SCALE, Math.max(MIN_SCALE, +(s + dir * 0.25).toFixed(2))));
  }, []);

  // 渲染结束按锚点恢复滚动中心（scale/rotation 不变的首渲没有锚点，跳过）。
  useEffect(() => {
    if (rendering) return;
    const a = anchorRef.current;
    const stage = stageRef.current;
    if (!a || !stage) return;
    anchorRef.current = null;
    stage.scrollLeft = a.fx * stage.scrollWidth - stage.clientWidth / 2;
    stage.scrollTop = a.fy * stage.scrollHeight - stage.clientHeight / 2;
  }, [rendering]);

  // Ctrl+滚轮缩放：React 的 wheel 委托是 passive 的，preventDefault 无效，
  // 必须原生监听 { passive: false } 才能拦下浏览器的页面缩放。
  useEffect(() => {
    const stage = stageRef.current;
    if (!stage) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      zoomStep(e.deltaY < 0 ? 1 : -1);
    };
    stage.addEventListener("wheel", onWheel, { passive: false });
    return () => stage.removeEventListener("wheel", onWheel);
  }, [zoomStep]);

  // 键盘快捷缩放：Ctrl+= / Ctrl+- 步进，Ctrl+0 回适应宽度。挂 window 而
  // 不是 stage：canvas 上没有焦点概念，焦点在列表里也应可用。
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
        setScale(1);
        setRotation(0);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [zoomStep]);

  useEffect(() => {
    void render();
    return () => renderTaskRef.current?.cancel();
  }, [render, pageCount]);

  if (error) {
    return <div className="preview-state-error">{error}</div>;
  }

  return (
    <div className="preview-pdf">
      <div className="preview-pdf-bar">
        <button
          type="button"
          className="head-action"
          disabled={pageNo <= 1}
          onClick={() => setPageNo((n) => Math.max(1, n - 1))}
          aria-label="上一页"
        >
          <ChevronLeft className="icon-md" />
        </button>
        <span className="preview-pdf-page">
          {pageCount ? `${pageNo} / ${pageCount}` : "载入中…"}
        </span>
        <button
          type="button"
          className="head-action"
          disabled={pageCount === 0 || pageNo >= pageCount}
          onClick={() => setPageNo((n) => Math.min(pageCount, n + 1))}
          aria-label="下一页"
        >
          <ChevronRight className="icon-md" />
        </button>
        <span className="preview-pdf-spacer" />
        <button
          type="button"
          className="head-action"
          onClick={() => setScale((s) => Math.max(MIN_SCALE, +(s - 0.25).toFixed(2)))}
          aria-label="缩小（Ctrl+- 或 Ctrl+滚轮）"
          title="缩小（Ctrl+- 或 Ctrl+滚轮）"
        >
          <ZoomOut className="icon-md" />
        </button>
        <span className="preview-pdf-zoom">{Math.round(scale * 100)}%</span>
        <button
          type="button"
          className="head-action"
          onClick={() => setScale((s) => Math.min(MAX_SCALE, +(s + 0.25).toFixed(2)))}
          aria-label="放大（Ctrl+= 或 Ctrl+滚轮）"
          title="放大（Ctrl+= 或 Ctrl+滚轮）"
        >
          <ZoomIn className="icon-md" />
        </button>
        <button
          type="button"
          className="head-action"
          onClick={() => setRotation((r) => (r + 90) % 360)}
          aria-label={`旋转 90°（当前 ${rotation}°）`}
          title="顺时针旋转 90°（Ctrl+0 复位）"
        >
          <RotateCw className="icon-md" />
        </button>
      </div>
      <div ref={stageRef} className="preview-pdf-stage" data-busy={rendering || undefined}>
        <canvas ref={canvasRef} />
      </div>
    </div>
  );
}
