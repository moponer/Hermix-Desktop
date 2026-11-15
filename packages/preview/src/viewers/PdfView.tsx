// PDF 渲染：pdf.js 直接画到 canvas。
//
// 一页一张 canvas（pdf.js 的渲染粒度就是页，没有跨页画布这回事），
// 版面与滚动行为由 DocPager 统一提供：连续纵向滚动 + 缩略图侧栏 + 惰性渲染。
//
// 已知离线代价：标准 14 号字要从 standardFontDataUrl 取度量，否则字宽算不
// 出来（文本层点不中）、渲染也会走回退字体。

import { useEffect, useRef, useState } from "react";
import * as pdfjsLib from "pdfjs-dist";
import type { PDFDocumentProxy } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { standardFontDataUrl } from "../lib/standardFonts";
import { DocPager, type PageSize } from "./DocPager";

pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

/** 一页画布：尺寸由外壳给定（缩放/旋转后），换尺寸就重画一帧。 */
function PdfPageCanvas({
  doc,
  pageNo,
  width,
  height,
  rotation,
}: {
  doc: PDFDocumentProxy;
  pageNo: number;
  width: number;
  height: number;
  /** 用户的附加旋转（页面自带 /Rotate 由 pdf.js 自己算）。 */
  rotation: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const taskRef = useRef<{ cancel: () => void } | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || width <= 0 || height <= 0) return;
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
        // 取消渲染（换尺寸 / 卸载）会走到这里，属正常流程
      }
    })();
    return () => {
      cancelled = true;
      taskRef.current?.cancel();
    };
  }, [doc, pageNo, width, height, rotation]);

  return <canvas ref={canvasRef} />;
}

export function PdfView({ bytes }: { bytes: Uint8Array }) {
  const [doc, setDoc] = useState<PDFDocumentProxy | null>(null);
  const [sizes, setSizes] = useState<PageSize[]>([]);
  const [error, setError] = useState<string | null>(null);

  // 打开文档。bytes 每次都是新引用，副本必须在这里做——pdf.js 会转移
  // ArrayBuffer 的所有权，直接传原始 bytes 会让它后续变成 detached。
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setDoc(null);
    setSizes([]);
    const copy = new Uint8Array(bytes).buffer;
    // 持有 loadingTask 而不是只拿 doc：销毁入口在 task 上，它同时会结束 worker。
    const loadingTask = pdfjsLib.getDocument({
      data: copy,
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

  // 各页尺寸（含页面自带 /Rotate）。铺版要先把每页占多高算出来，否则滚动条
  // 会随渲染一截一截长。取的是页面字典，不解码内容，代价很小。
  useEffect(() => {
    if (!doc) return;
    let cancelled = false;
    void (async () => {
      const out: PageSize[] = [];
      for (let n = 1; n <= doc.numPages; n++) {
        if (cancelled) return;
        try {
          const page = await doc.getPage(n);
          const vp = page.getViewport({ scale: 1, rotation: page.rotate });
          out.push({ w: vp.width, h: vp.height });
        } catch {
          out.push({ w: 595, h: 842 }); // 单页取不到时不至于让整份文档塌掉
        }
      }
      if (!cancelled) setSizes(out);
    })();
    return () => {
      cancelled = true;
    };
  }, [doc]);

  if (error) return <div className="preview-state-error">{error}</div>;

  return (
    <DocPager
      count={doc?.numPages ?? 0}
      sizes={sizes}
      rotatable
      loadingText="正在载入页面…"
      renderPage={(n, w, h, rotation) =>
        doc ? <PdfPageCanvas doc={doc} pageNo={n} width={w} height={h} rotation={rotation} /> : null
      }
      renderThumb={(n, w, h, rotation) =>
        doc ? <PdfPageCanvas doc={doc} pageNo={n} width={w} height={h} rotation={rotation} /> : null
      }
    />
  );
}
