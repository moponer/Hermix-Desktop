// 「改字」模式的文本层：把 pdf.js 的文本项渲染成**可点选的覆盖层**。
//
// 为什么要这一层：PDF 里没有"段落"，只有绝对定位的绘制指令。想知道
// 「用户说的那句字在页面的哪个位置」，唯一可靠的办法是拿**文本项的位置**
// —— pdf.js 的 getTextContent() 正好给这个（每项：字符串 + 变换矩阵）。
//
// 有了位置，后端就能"盖住那块 + 写新字"，完全绕开"内容流里那串字节
// 怎么编码、怎么被拆成 TJ 数组"这些坑（详见决策笔记 P0/P1）。

import { useCallback, useEffect, useRef, useState } from "react";
import * as pdfjsLib from "pdfjs-dist";
import type { PDFDocumentProxy, PageViewport } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";

pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

/** 选中的一段文字（坐标一律 PDF 用户空间）。 */
export interface PickedText {
  /** 原文 */
  text: string;
  /** 要盖住的矩形：[x0, y0, x1, y1] */
  rect: [number, number, number, number];
  /** 新文字的基线起点 */
  x: number;
  y: number;
  /** 字号（pt） */
  size: number;
}

interface LayerItem extends PickedText {
  id: string;
  /** 覆盖层里的位置（viewport CSS px） */
  left: number;
  top: number;
  width: number;
  height: number;
}

export function PdfTextPicker({
  doc,
  page,
  pickedId,
  onPick,
}: {
  doc: PDFDocumentProxy | null;
  page: number;
  pickedId: string | null;
  onPick: (item: PickedText & { id: string }) => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const viewportRef = useRef<PageViewport | null>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [items, setItems] = useState<LayerItem[]>([]);

  // 渲染页面
  useEffect(() => {
    if (!doc) return;
    let cancelled = false;
    void (async () => {
      try {
        const p = await doc.getPage(page);
        if (cancelled) return;
        const base = p.getViewport({ scale: 1 });
        const stage = stageRef.current;
        const fit = stage ? (stage.clientWidth - 24) / base.width : 1;
        const safeFit = fit > 0.1 ? fit : 1;
        const cssVp = p.getViewport({ scale: safeFit });
        viewportRef.current = cssVp;
        setSize({ w: cssVp.width, h: cssVp.height });
        const canvas = canvasRef.current;
        if (!canvas) return;
        const dpr = window.devicePixelRatio || 1;
        const vp = p.getViewport({ scale: safeFit * dpr });
        canvas.width = Math.floor(vp.width);
        canvas.height = Math.floor(vp.height);
        canvas.style.width = `${Math.floor(vp.width / dpr)}px`;
        canvas.style.height = `${Math.floor(vp.height / dpr)}px`;
        await p.render({ canvas, viewport: vp }).promise;
      } catch {
        /* 单页渲染失败保留上一帧 */
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [doc, page, size.w]);

  // 文本层：把每个文本项的落点算出来（PDF 用户空间 + viewport 像素）
  useEffect(() => {
    const doc0 = doc;
    if (!doc0) return;
    let cancelled = false;
    void (async () => {
      try {
        const p = await doc0.getPage(page);
        if (cancelled) return;
        const vp = p.getViewport({ scale: 1 });
        const content = await p.getTextContent();
        if (cancelled) return;
        const scale = viewportRef.current?.scale ?? 1;
        const out: LayerItem[] = [];
        content.items.forEach((item, i) => {
          if (!("str" in item) || !item.str.trim()) return;
          const tx = pdfjsLib.Util.transform(vp.transform, item.transform);
          const fontHeight = Math.hypot(tx[2], tx[3]) * scale;
          const left = tx[4];
          const top = tx[5] - fontHeight * 0.8;
          const width = item.width * scale;
          if (width <= 0 || fontHeight <= 0) return;
          // viewport 像素 → PDF 用户空间（PDF 的 y 轴朝上，转换会翻）。
          const p1 = vp.convertToPdfPoint(left, top);
          const p2 = vp.convertToPdfPoint(left + width, top + fontHeight);
          const base = vp.convertToPdfPoint(left, tx[5]);
          out.push({
            id: `t${i}`,
            text: item.str,
            rect: [
              Math.min(p1[0], p2[0]),
              Math.min(p1[1], p2[1]),
              Math.max(p1[0], p2[0]),
              Math.max(p1[1], p2[1]),
            ],
            x: base[0],
            y: base[1],
            size: Math.max(4, Math.min(72, fontHeight / scale)),
            left,
            top,
            width,
            height: fontHeight,
          });
        });
        setItems(out);
      } catch {
        setItems([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [doc, page, size.w]);

  const onClickItem = useCallback(
    (item: LayerItem) => {
      onPick({ id: item.id, text: item.text, rect: item.rect, x: item.x, y: item.y, size: item.size });
    },
    [onPick],
  );

  return (
    <div className="pa-stage" ref={stageRef}>
      <div className="pa-page" style={{ width: size.w || undefined }}>
        <canvas ref={canvasRef} className="pa-canvas" />
        <div className="pa-textlayer" style={{ width: size.w, height: size.h }}>
          {items.map((it) => (
            <button
              key={it.id}
              type="button"
              className={pickedId === it.id ? "pa-textitem is-sel" : "pa-textitem"}
              style={{
                left: it.left,
                top: it.top,
                width: it.width,
                height: it.height,
                fontSize: it.height,
              }}
              onClick={() => onClickItem(it)}
              title={it.text}
            >
              {it.text}
            </button>
          ))}
        </div>
      </div>
    </div>
  );
}
