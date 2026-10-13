// PDF 渲染：pdf.js 直接画到 canvas。
//
// 之前用 `<embed type="application/pdf">`，等于把渲染交给 WebView 内置
// 查看器——没有页码、没有缩放、没法选中文本，且在不同机器上表现不一致
// （有的 WebView2 根本不渲染）。换成 pdf.js 后这些都在我们自己手里。

import { useCallback, useEffect, useRef, useState } from "react";
import * as pdfjsLib from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { ChevronLeft, ChevronRight, ZoomIn, ZoomOut } from "lucide-react";

pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

const MIN_SCALE = 0.5;
const MAX_SCALE = 3;

export function PdfView({ bytes }: { bytes: Uint8Array }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const docRef = useRef<PDFDocumentProxy | null>(null);
  const taskRef = useRef<ReturnType<typeof pdfjsLib.getDocument> | null>(null);
  const renderTaskRef = useRef<{ cancel: () => void } | null>(null);

  const [pageCount, setPageCount] = useState(0);
  const [pageNo, setPageNo] = useState(1);
  const [scale, setScale] = useState(1);
  const [error, setError] = useState<string | null>(null);
  const [rendering, setRendering] = useState(false);

  // 打开文档。bytes 每次都是新引用，副本必须在这里做——pdf.js 会转移
  // ArrayBuffer 的所有权，直接传原始 bytes 会让它后续变成 detached。
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setPageCount(0);
    const copy = new Uint8Array(bytes).buffer;
    // 持有 loadingTask 而不是只拿 doc：销毁入口在 task 上，它同时会
    // 结束 worker。只丢 doc 引用会让解析线程留着直到 GC。
    const loadingTask = pdfjsLib.getDocument({ data: copy });
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
      // 先按容器宽度定基准，再乘缩放——这样 scale=1 就是"适应宽度"
      const base = page.getViewport({ scale: 1 });
      const fit = canvas.parentElement
        ? (canvas.parentElement.clientWidth - 32) / base.width
        : 1;
      const dpr = window.devicePixelRatio || 1;
      const viewport = page.getViewport({ scale: fit * scale * dpr });

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
  }, [pageNo, scale]);

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
          aria-label="缩小"
        >
          <ZoomOut className="icon-md" />
        </button>
        <span className="preview-pdf-zoom">{Math.round(scale * 100)}%</span>
        <button
          type="button"
          className="head-action"
          onClick={() => setScale((s) => Math.min(MAX_SCALE, +(s + 0.25).toFixed(2)))}
          aria-label="放大"
        >
          <ZoomIn className="icon-md" />
        </button>
      </div>
      <div className="preview-pdf-stage" data-busy={rendering || undefined}>
        <canvas ref={canvasRef} />
      </div>
    </div>
  );
}
