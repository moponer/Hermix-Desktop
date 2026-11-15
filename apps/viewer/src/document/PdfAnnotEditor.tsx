// PDF 标注编辑器 —— 在页面上画自由文字 / 高亮 / 墨迹，写回 PDF 的注释层。
//
// ## 为什么自己画而不是用 pdf.js 官方 AnnotationEditor
//
// 官方引擎（`AnnotationEditorLayer` + `AnnotationEditorUIManager`）写出来
// 的就是 **/FreeText、/Highlight、/Ink** 这三种标准注释 —— 我们写进文件的
// 是同一批类型，Acrobat / 福昕 / Edge / pdf.js 都能开、能继续编辑。
//
// 但官方引擎**离不开官方外壳**：`AnnotationEditorLayer` 对 UI 管理器有 40+
// 处调用（getMode / combinedSignal / commitOrRemove / addToAnnotationStorage
// / undo 栈 / eventBus…），而那个管理器的构造函数要 16 个参数、绑着官方
// eventBus、altText 与 signature 管理器、undo bar。把它单独抽出来等于
// 重写半个官方查看器。官方外壳又会把整个 PDF 界面换成 pdf.js 的样子，
// 与本应用的视觉体系冲突。
//
// 所以：**引擎的产物用标准注释复刻，交互与视觉自己做**。
//
// ## 坐标
//
// 一切坐标都是 **PDF 用户空间**（左下原点、单位 pt）。屏幕坐标经
// pdf.js 的 `viewport.convertToPdfPoint()` 换算；反向用
// `convertToViewportPoint()`（把待写入的注释画回屏幕上）。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import * as pdfjsLib from "pdfjs-dist";
import type { PDFDocumentProxy, PageViewport } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { Type, Highlighter, Pencil, Trash2, Image as ImageIcon, PenLine, FileSignature } from "lucide-react";

pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

export type AnnotTool =
  | "text"
  | "highlight"
  | "ink"
  | "stamp"
  | "signature"
  | "sigfield"
  | "select";

/** 待写入的标注（坐标一律 PDF 用户空间）。 */
export type PendingAnnot = {
  id: string;
  kind: "freeText" | "highlight" | "ink" | "stamp" | "signature" | "signatureField";
  rect: [number, number, number, number];
  contents?: string;
  fontSize?: number;
  color?: [number, number, number];
  quadPoints?: number[][];
  /** 墨迹：笔画数组，每条笔画是一串 [x, y] 点。 */
  inkList?: number[][][];
  width?: number;
  /** 图章：本地图片路径（Rust 侧读文件）+ 前端预览用的 object URL。 */
  imagePath?: string;
  previewUrl?: string;
  /** 签名：PNG 的 base64（Rust 侧解成图片）+ 预览 URL。 */
  signatureBase64?: string;
  fieldName?: string;
};

const HIGHLIGHT_COLOR: [number, number, number] = [1, 0.92, 0.23];
const INK_COLOR: [number, number, number] = [0.85, 0.15, 0.15];
const TEXT_COLOR: [number, number, number] = [0.1, 0.2, 0.8];

let seq = 0;
function nextId(): string {
  seq += 1;
  return `a${seq}`;
}

/** 颜色 → CSS（0..1 → 0..255）。 */
function cssColor(c: [number, number, number]): string {
  const to = (v: number) => Math.round(Math.max(0, Math.min(1, v)) * 255);
  return `rgb(${to(c[0])}, ${to(c[1])}, ${to(c[2])})`;
}

export function AnnotCanvas({
  doc,
  page,
  tool,
  annots,
  onAdd,
  onSelect,
  selectedId,
  stampImage,
  signature,
  fieldName,
}: {
  doc: PDFDocumentProxy | null;
  page: number;
  tool: AnnotTool;
  annots: PendingAnnot[];
  onAdd: (a: PendingAnnot) => void;
  onSelect: (id: string | null) => void;
  selectedId: string | null;
  /** 已选中的图章图片（路径 + 预览 URL）；没选时图章工具不可用。 */
  stampImage: { path: string; url: string } | null;
  /** 已写好的签名（base64 + 预览 URL）；没写时签名工具不可用。 */
  signature: { base64: string; url: string } | null;
  /** 签名域的名字（写进 PDF 的 /T，别人在 Acrobat 里看到的就是它）。 */
  fieldName: string;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const stageRef = useRef<HTMLDivElement | null>(null);
  const viewportRef = useRef<PageViewport | null>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });

  /** 正在画的形状（未提交）。 */
  const [draft, setDraft] = useState<
    | { kind: "highlight"; rect: [number, number, number, number] }
    | { kind: "ink"; points: number[][] }
    | { kind: "stamp"; rect: [number, number, number, number] }
    | { kind: "signature"; rect: [number, number, number, number] }
    | { kind: "sigfield"; rect: [number, number, number, number] }
    | null
  >(null);
  /** 文字框：点一下先出输入框，输入完才落成标注。 */
  const [textDraft, setTextDraft] = useState<{ x: number; y: number; value: string } | null>(
    null,
  );
  const startRef = useRef<{ x: number; y: number } | null>(null);

  // 渲染当前页
  useEffect(() => {
    if (!doc) return;
    let cancelled = false;
    void (async () => {
      try {
        const p = await doc.getPage(page);
        if (cancelled) return;
        const base = p.getViewport({ scale: 1 });
        // 宽度参考**舞台容器**，不是 .pa-page —— 后者的宽度正是这里要算出来的，
        // 首帧拿它算会得到负数 fit（画布尺寸为负、整页空白）。
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

  /** 屏幕坐标 → PDF 坐标。 */
  const toPdf = useCallback((clientX: number, clientY: number): [number, number] | null => {
    const vp = viewportRef.current;
    const el = canvasRef.current;
    if (!vp || !el) return null;
    const rect = el.getBoundingClientRect();
    const [x, y] = vp.convertToPdfPoint(clientX - rect.left, clientY - rect.top);
    return [x, y];
  }, []);

  /** PDF 坐标 → 屏幕（CSS px）坐标，用于把待写入的注释画回屏幕。 */
  const toScreen = useCallback((x: number, y: number): [number, number] => {
    const vp = viewportRef.current;
    if (!vp) return [0, 0];
    const [sx, sy] = vp.convertToViewportPoint(x, y);
    return [sx, sy];
  }, []);

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (tool === "select") {
      onSelect(null);
      return;
    }
    const pt = toPdf(e.clientX, e.clientY);
    if (!pt) return;
    if (tool === "text") {
      setTextDraft({ x: pt[0], y: pt[1], value: "" });
      return;
    }
    startRef.current = { x: pt[0], y: pt[1] };
    if (tool === "highlight") {
      setDraft({ kind: "highlight", rect: [pt[0], pt[1], pt[0], pt[1]] });
    } else if (tool === "stamp") {
      if (!stampImage) return;
      setDraft({ kind: "stamp", rect: [pt[0], pt[1], pt[0], pt[1]] });
    } else if (tool === "signature") {
      if (!signature) return;
      setDraft({ kind: "signature", rect: [pt[0], pt[1], pt[0], pt[1]] });
    } else if (tool === "sigfield") {
      // 空签名域：不需要先写签名，落框就是域。
      setDraft({ kind: "sigfield", rect: [pt[0], pt[1], pt[0], pt[1]] });
    } else if (tool === "ink") {
      setDraft({ kind: "ink", points: [[pt[0], pt[1]]] });
    }
  };

  const onPointerMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!startRef.current) return;
    const pt = toPdf(e.clientX, e.clientY);
    if (!pt) return;
    if (tool === "highlight" || tool === "stamp" || tool === "signature" || tool === "sigfield") {
      // 注意：**只能更新 rect，不能改 kind**。此前这里把 kind 写死成
      // "highlight"，于是拖框放图章/签名时草稿被改成高亮 —— 提交出去的是
      // 一条黄色高亮，不是图章/签名。
      setDraft((d) => {
        if (!d || d.kind === "ink") return d;
        return {
          ...d,
          rect: [
            Math.min(startRef.current!.x, pt[0]),
            Math.min(startRef.current!.y, pt[1]),
            Math.max(startRef.current!.x, pt[0]),
            Math.max(startRef.current!.y, pt[1]),
          ],
        } as typeof d;
      });
    } else if (tool === "ink") {
      setDraft((d) =>
        d && d.kind === "ink" ? { kind: "ink", points: [...d.points, [pt[0], pt[1]]] } : d,
      );
    }
  };

  const onPointerUp = () => {
    const d = draft;
    startRef.current = null;
    setDraft(null);
    if (!d) return;
    if (d.kind === "signature") {
      const [x0, y0, x1, y1] = d.rect;
      if (x1 - x0 < 8 || y1 - y0 < 8) return; // 误点
      if (!signature) return;
      onAdd({
        id: nextId(),
        kind: "signature",
        rect: d.rect,
        signatureBase64: signature.base64,
        previewUrl: signature.url,
        fieldName: fieldName || "签名",
      });
    } else if (d.kind === "sigfield") {
      const [x0, y0, x1, y1] = d.rect;
      if (x1 - x0 < 8 || y1 - y0 < 8) return; // 误点
      onAdd({
        id: nextId(),
        kind: "signatureField",
        rect: d.rect,
        fieldName: fieldName || "签名",
      });
    } else if (d.kind === "stamp") {
      const [x0, y0, x1, y1] = d.rect;
      if (x1 - x0 < 8 || y1 - y0 < 8) return; // 误点
      if (!stampImage) return;
      onAdd({
        id: nextId(),
        kind: "stamp",
        rect: d.rect,
        imagePath: stampImage.path,
        previewUrl: stampImage.url,
      });
    } else if (d.kind === "highlight") {
      const [x0, y0, x1, y1] = d.rect;
      if (x1 - x0 < 3 || y1 - y0 < 3) return; // 误点
      // QuadPoints 的顺序是：左上、右上、左下、右下。
      onAdd({
        id: nextId(),
        kind: "highlight",
        rect: d.rect,
        quadPoints: [[x0, y1, x1, y1, x0, y0, x1, y0]],
        color: HIGHLIGHT_COLOR,
      });
    } else {
      if (d.points.length < 2) return;
      const xs = d.points.map((p) => p[0]);
      const ys = d.points.map((p) => p[1]);
      const rect: [number, number, number, number] = [
        Math.min(...xs),
        Math.min(...ys),
        Math.max(...xs),
        Math.max(...ys),
      ];
      onAdd({
        id: nextId(),
        kind: "ink",
        rect,
        inkList: [d.points],
        color: INK_COLOR,
        width: 2,
      });
    }
  };

  /** 文字框提交：把输入内容变成一条 FreeText。 */
  const commitText = () => {
    const t = textDraft;
    setTextDraft(null);
    if (!t || !t.value.trim()) return;
    const size = 14;
    onAdd({
      id: nextId(),
      kind: "freeText",
      // 文本框从落点往上长（PDF 的 y 轴朝上）。
      rect: [t.x, t.y, t.x + 220, t.y + size * 1.4],
      contents: t.value.trim(),
      fontSize: size,
      color: TEXT_COLOR,
    });
  };

  const overlays = useMemo(
    () =>
      annots.map((a) => {
        const [x0, y0] = toScreen(a.rect[0], a.rect[1]);
        const [x1, y1] = toScreen(a.rect[2], a.rect[3]);
        return {
          a,
          left: Math.min(x0, x1),
          top: Math.min(y0, y1),
          width: Math.abs(x1 - x0),
          height: Math.abs(y1 - y0),
        };
      }),
    [annots, toScreen],
  );

  const inkPaths = useMemo(
    () =>
      annots
        .filter((a) => a.kind === "ink" && a.inkList?.length)
        .map((a) => ({
          id: a.id,
          color: cssColor(a.color ?? INK_COLOR),
          d: (a.inkList ?? [])
            .map(
              (stroke) =>
                stroke
                  .map((p, i) => {
                    const [sx, sy] = toScreen(p[0], p[1]);
                    return `${i === 0 ? "M" : "L"}${sx.toFixed(1)},${sy.toFixed(1)}`;
                  })
                  .join(" "),
            )
            .join(" "),
        })),
    [annots, toScreen],
  );

  return (
    <div className="pa-stage" data-tool={tool} ref={stageRef}>
      <div className="pa-page" style={{ width: size.w || undefined }}>
        <canvas ref={canvasRef} className="pa-canvas" />
        <div
          className="pa-overlay"
          style={{ width: size.w, height: size.h }}
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerLeave={onPointerUp}
        >
          {overlays.map((o) => {
            const isSel = o.a.id === selectedId;
            if (o.a.kind === "highlight") {
              return (
                <div
                  key={o.a.id}
                  className={isSel ? "pa-mark is-sel" : "pa-mark"}
                  style={{
                    left: o.left,
                    top: o.top,
                    width: o.width,
                    height: o.height,
                    background: cssColor(o.a.color ?? HIGHLIGHT_COLOR),
                  }}
                  onPointerDown={(e) => {
                    if (tool === "select") {
                      e.stopPropagation();
                      onSelect(o.a.id);
                    }
                  }}
                />
              );
            }
            if (o.a.kind === "signature") {
              return (
                <img
                  key={o.a.id}
                  className={isSel ? "pa-stamp is-sel" : "pa-stamp"}
                  src={o.a.previewUrl}
                  alt="签名"
                  style={{ left: o.left, top: o.top, width: o.width, height: o.height }}
                  onPointerDown={(e) => {
                    if (tool === "select") {
                      e.stopPropagation();
                      onSelect(o.a.id);
                    }
                  }}
                />
              );
            }
            if (o.a.kind === "signatureField") {
              // 空签名域：画出来是一个占位框（真实 PDF 里是外观流画的边框）。
              return (
                <div
                  key={o.a.id}
                  className={isSel ? "pa-sigfield is-sel" : "pa-sigfield"}
                  style={{ left: o.left, top: o.top, width: o.width, height: o.height }}
                  onPointerDown={(e) => {
                    if (tool === "select") {
                      e.stopPropagation();
                      onSelect(o.a.id);
                    }
                  }}
                >
                  {o.a.fieldName || "签名"}
                </div>
              );
            }
            if (o.a.kind === "stamp") {
              return (
                <img
                  key={o.a.id}
                  className={isSel ? "pa-stamp is-sel" : "pa-stamp"}
                  src={o.a.previewUrl}
                  alt="图章"
                  style={{ left: o.left, top: o.top, width: o.width, height: o.height }}
                  onPointerDown={(e) => {
                    if (tool === "select") {
                      e.stopPropagation();
                      onSelect(o.a.id);
                    }
                  }}
                />
              );
            }
            if (o.a.kind === "freeText") {
              return (
                <div
                  key={o.a.id}
                  className={isSel ? "pa-text is-sel" : "pa-text"}
                  style={{
                    left: o.left,
                    top: o.top,
                    color: cssColor(o.a.color ?? TEXT_COLOR),
                    fontSize: o.a.fontSize ?? 14,
                  }}
                  onPointerDown={(e) => {
                    if (tool === "select") {
                      e.stopPropagation();
                      onSelect(o.a.id);
                    }
                  }}
                >
                  {o.a.contents}
                </div>
              );
            }
            return null;
          })}
          <svg className="pa-svg" width={size.w} height={size.h}>
            {inkPaths.map((p) => (
              <path
                key={p.id}
                d={p.d}
                stroke={p.color}
                strokeWidth={2}
                fill="none"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            ))}
            {draft?.kind === "ink" && (
              <path
                d={draft.points
                  .map(
                    (p, i) =>
                      `${i === 0 ? "M" : "L"}${toScreen(p[0], p[1])[0].toFixed(1)},${toScreen(
                        p[0],
                        p[1],
                      )[1].toFixed(1)}`,
                  )
                  .join(" ")}
                stroke={cssColor(INK_COLOR)}
                strokeWidth={2}
                fill="none"
                strokeLinecap="round"
              />
            )}
            {draft?.kind === "highlight" && (
              <rect
                x={Math.min(toScreen(draft.rect[0], draft.rect[1])[0], toScreen(draft.rect[2], draft.rect[3])[0])}
                y={Math.min(toScreen(draft.rect[0], draft.rect[1])[1], toScreen(draft.rect[2], draft.rect[3])[1])}
                width={Math.abs(toScreen(draft.rect[2], draft.rect[3])[0] - toScreen(draft.rect[0], draft.rect[1])[0])}
                height={Math.abs(toScreen(draft.rect[3], draft.rect[1])[1] - toScreen(draft.rect[0], draft.rect[1])[1])}
                fill={cssColor(HIGHLIGHT_COLOR)}
                opacity={0.45}
              />
            )}
          </svg>
          {textDraft && (
            <input
              className="pa-input"
              autoFocus
              value={textDraft.value}
              placeholder="写点什么"
              style={{
                left: toScreen(textDraft.x, textDraft.y)[0],
                top: toScreen(textDraft.x, textDraft.y)[1],
                color: cssColor(TEXT_COLOR),
              }}
              onChange={(e) => setTextDraft({ ...textDraft, value: e.target.value })}
              onBlur={commitText}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  commitText();
                } else if (e.key === "Escape") {
                  setTextDraft(null);
                }
              }}
            />
          )}
        </div>
      </div>
    </div>
  );
}

/** 工具条上的四个按钮。 */
export function AnnotTools({
  tool,
  onTool,
  onDelete,
  canDelete,
  onPickStamp,
  stampName,
}: {
  tool: AnnotTool;
  onTool: (t: AnnotTool) => void;
  onDelete: () => void;
  canDelete: boolean;
  /** 打开系统文件对话框选图章图片。 */
  onPickStamp: () => void;
  /** 已选中的图章图片名（空 = 还没选）。 */
  stampName: string | null;
}) {
  const items: { id: AnnotTool; label: string; icon: React.ReactNode }[] = [
    { id: "text", label: "文字", icon: <Type className="icon-sm" /> },
    { id: "highlight", label: "高亮", icon: <Highlighter className="icon-sm" /> },
    { id: "ink", label: "墨迹", icon: <Pencil className="icon-sm" /> },
    { id: "stamp", label: "图章", icon: <ImageIcon className="icon-sm" /> },
    { id: "signature", label: "签名", icon: <PenLine className="icon-sm" /> },
    { id: "sigfield", label: "签名域", icon: <FileSignature className="icon-sm" /> },
  ];
  return (
    <div className="pa-tools">
      {items.map((it) => (
        <button
          key={it.id}
          type="button"
          className={tool === it.id ? "tb-btn is-active" : "tb-btn"}
          onClick={() => onTool(it.id)}
          title={it.label}
          aria-label={it.label}
        >
          {it.icon} {it.label}
        </button>
      ))}
      <button
        type="button"
        className={tool === "stamp" ? "tb-btn is-active" : "tb-btn"}
        onClick={onPickStamp}
        title={stampName ? `图章图片：${stampName}（点击更换）` : "选择一张图章图片"}
        aria-label="选择图章图片"
      >
        <ImageIcon className="icon-sm" /> {stampName ? "换图章" : "选图章"}
      </button>
      <button
        type="button"
        className="tb-btn"
        onClick={onDelete}
        disabled={!canDelete}
        title="删除选中的标注"
        aria-label="删除选中的标注"
      >
        <Trash2 className="icon-sm" /> 删除
      </button>
    </div>
  );
}

/** 待写入标注 → Rust 命令的载荷（去掉前端自己的 id）。 */
export function toWirePayload(annots: PendingAnnot[]): unknown[] {
  return annots.map((a) => {
    if (a.kind === "freeText") {
      return {
        kind: "freeText",
        rect: a.rect,
        contents: a.contents ?? "",
        fontSize: a.fontSize ?? 14,
        color: a.color ?? TEXT_COLOR,
      };
    }
    if (a.kind === "highlight") {
      return {
        kind: "highlight",
        rect: a.rect,
        quadPoints: a.quadPoints ?? [],
        color: a.color ?? HIGHLIGHT_COLOR,
      };
    }
    if (a.kind === "stamp") {
      return { kind: "stamp", rect: a.rect, imagePath: a.imagePath ?? "" };
    }
    if (a.kind === "signature") {
      return {
        kind: "signature",
        rect: a.rect,
        imageBase64: a.signatureBase64 ?? "",
        fieldName: a.fieldName ?? "签名",
      };
    }
    if (a.kind === "signatureField") {
      return {
        kind: "signatureField",
        rect: a.rect,
        fieldName: a.fieldName ?? "签名",
      };
    }
    return {
      kind: "ink",
      rect: a.rect,
      inkList: a.inkList ?? [],
      color: a.color ?? INK_COLOR,
      width: a.width ?? 2,
    };
  });
}
