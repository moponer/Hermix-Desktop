// PDF 页面编辑器（重量模式专属）。
//
// 形态：左侧页列表 + 右侧操作面板。用户改的是**页列表**（删/移/旋转/
// 插空白），导出时把页列表编译成一串 PDF op 交给 Rust 一次执行。
//
// 页号语义（最容易错的地方，钉死三条）：
//   1. 列表里每一项要么是「原第几页」，要么是「空白页」。
//   2. 编译出的 op 顺序固定为：keep（删页 + 重排一次搞定）→ insertBlank
//      （按最终位置**从小到大**插，先插小的才不会顶掉后面页码）→
//      rotate（用**最终页号**）。
//   3. 「最终页号」= 该项在列表里的下标 + 1。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronUp, ChevronDown, RotateCw, Save, FolderOpen, FilePlus2, Wrench, MessageSquarePlus } from "lucide-react";
import * as pdfjsLib from "pdfjs-dist";
import type { PDFDocumentProxy, PDFPageProxy, PageViewport } from "pdfjs-dist";
import workerUrl from "pdfjs-dist/build/pdf.worker.min.mjs?url";
import { standardFontDataUrl } from "@hermix/preview";
import type { FileSource } from "@hermix/preview";
import { PdfToolsDialog } from "./PdfToolsDialog";
import { SignaturePad } from "./SignaturePad";
import { PdfTextPicker, type PickedText } from "./PdfTextPicker";
import {
  AnnotCanvas,
  AnnotTools,
  toWirePayload,
  type AnnotTool,
  type PendingAnnot,
} from "./PdfAnnotEditor";

pdfjsLib.GlobalWorkerOptions.workerSrc = workerUrl;

/** 一个页面项：原页号（1-based）或空白页（source = null）。 */
interface PageEntry {
  source: number | null;
  rotation: number;
}

interface PdfInfo {
  path: string;
  pages: number;
  encrypted: boolean;
  bytes: number;
  /** 每页当前的显示角度（0/90/180/270，含继承来的）。 */
  rotations: number[];
}

interface SaveResult {
  path: string;
  bytes: number;
  encodingUsed: string | null;
  /** 实际替换的处数（仅文本替换有值）。 */
  replaced?: number | null;
  /** 需要如实告诉用户的话（例如"旧字未能真删除"）。 */
  note?: string | null;
}

/** 与 Rust `PdfOp` 对应（serde tag = "op"，camelCase 变体名）。 */
type PdfOp =
  | { op: "delete"; pages: number[] }
  | { op: "keep"; pages: number[] }
  | { op: "reorder"; order: number[] }
  | { op: "rotate"; pages: number[]; degrees: number }
  | { op: "insertBlank"; at: number; width: number; height: number };

/** A4（PDF point）。空白页默认尺寸。 */
const A4_W = 595;
const A4_H = 842;

export function PdfPageEditor({
  path,
  name,
  source,
  onClose,
}: {
  path: string;
  name: string;
  source: FileSource;
  onClose: () => void;
}) {
  const [info, setInfo] = useState<PdfInfo | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pages, setPages] = useState<PageEntry[]>([]);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // ── 文本层替换 ──
  const [replacePage, setReplacePage] = useState(1);
  const [replaceFrom, setReplaceFrom] = useState("");
  const [replaceTo, setReplaceTo] = useState("");
  // ── 批注（便签）──
  const [annotText, setAnnotText] = useState("");
  const [annotPoint, setAnnotPoint] = useState<{ x: number; y: number } | null>(null);
  // ── 页面标注（自由文字 / 高亮 / 墨迹）──
  const [editMode, setEditMode] = useState<"pages" | "annot" | "text">("pages");
  /** 「改字」模式：当前选中的文本项。 */
  const [picked, setPicked] = useState<(PickedText & { id: string }) | null>(null);
  const [newText, setNewText] = useState("");
  const [tool, setTool] = useState<AnnotTool>("text");
  const [annots, setAnnots] = useState<PendingAnnot[]>([]);
  const [selectedAnnot, setSelectedAnnot] = useState<string | null>(null);
  /** 已选的图章图片（路径给 Rust，URL 给前端预览）。 */
  const [stampImage, setStampImage] = useState<{ path: string; url: string } | null>(null);
  /** 手写好的签名（base64 给 Rust，URL 给前端预览）。 */
  const [signature, setSignature] = useState<{ base64: string; url: string } | null>(null);
  /** 签名域的名字（写进 PDF 的 /T）。 */
  const [fieldName, setFieldName] = useState("签名");
  const [padOpen, setPadOpen] = useState(false);
  /** OCR 产出的文件路径（可打开）。 */
  const [ocrOut, setOcrOut] = useState<string | null>(null);
  // ── 本页文字清单（点一下就能填进「原文」，不用手打全串）──
  const [textItems, setTextItems] = useState<string[]>([]);
  const [pdfDoc, setPdfDoc] = useState<PDFDocumentProxy | null>(null);
  const docRef = useRef<PDFDocumentProxy | null>(null);
  const taskRef = useRef<ReturnType<typeof pdfjsLib.getDocument> | null>(null);
  const pageRef = useRef<PDFPageProxy | null>(null);
  const viewportRef = useRef<PageViewport | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // ── 文档工具（合并 / 裁剪 / 水印 / 页码 / 信息 / 密码）──
  const [toolsOpen, setToolsOpen] = useState(false);

  // 读一份 PDF 到 pdf.js：批注要"点哪放哪"，文字清单也要靠它的文本层。
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      const res = await source.readBytes(path);
      if (!res.ok || cancelled) return;
      try {
        const task = pdfjsLib.getDocument({
          data: new Uint8Array(res.bytes).buffer,
          ...(standardFontDataUrl ? { standardFontDataUrl } : {}),
        });
        taskRef.current = task;
        const doc = await task.promise;
        if (cancelled) {
          void task.destroy();
          return;
        }
        docRef.current = doc;
        setPdfDoc(doc);
      } catch {
        /* 解析不了就不给批注功能，其余编辑照旧 */
      }
    })();
    return () => {
      cancelled = true;
      void taskRef.current?.destroy();
      taskRef.current = null;
      docRef.current = null;
      setPdfDoc(null);
    };
  }, [path, source]);

  // 当前页：画预览 + 取文字清单。
  useEffect(() => {
    const doc = docRef.current;
    if (!doc) return;
    let cancelled = false;
    void (async () => {
      try {
        const page = await doc.getPage(replacePage);
        if (cancelled) return;
        pageRef.current = page;
        const viewport = page.getViewport({ scale: 1 });
        const canvas = canvasRef.current;
        if (canvas) {
          const fit = 300 / viewport.width;
          const dpr = window.devicePixelRatio || 1;
          const vp = page.getViewport({ scale: fit * dpr });
          canvas.width = Math.floor(vp.width);
          canvas.height = Math.floor(vp.height);
          canvas.style.width = `${Math.floor(vp.width / dpr)}px`;
          canvas.style.height = `${Math.floor(vp.height / dpr)}px`;
          viewportRef.current = page.getViewport({ scale: fit });
          await page.render({ canvas, viewport: vp }).promise;
        }
        const content = await page.getTextContent();
        if (cancelled) return;
        const seen = new Set<string>();
        const items: string[] = [];
        for (const item of content.items) {
          const s = "str" in item ? item.str : "";
          const t = s.trim();
          if (!t || seen.has(t)) continue;
          seen.add(t);
          items.push(t);
          if (items.length >= 200) break;
        }
        setTextItems(items);
      } catch {
        setTextItems([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [replacePage, info]);

  /** 点页面 → 记下 PDF 用户空间坐标（左下原点）。 */
  const onCanvasClick = useCallback((e: React.MouseEvent<HTMLCanvasElement>) => {
    const vp = viewportRef.current;
    if (!vp) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const pt = vp.convertToPdfPoint(e.clientX - rect.left, e.clientY - rect.top);
    if (pt) setAnnotPoint({ x: pt[0], y: pt[1] });
  }, []);

  /** 加一条便签批注：坐标与文字都交给 Rust 写进 PDF 的 /Annots。 */
  const doAnnotate = useCallback(async () => {
    if (!annotPoint) {
      setNote("先在下面的页面预览上点一下要放批注的位置");
      return;
    }
    if (!annotText.trim()) {
      setNote("请填写批注内容");
      return;
    }
    setBusy(true);
    setNote(null);
    try {
      const r = await invoke<SaveResult>("pdf_add_annotation", {
        srcPath: path,
        outPath: null,
        page: replacePage,
        x: annotPoint.x,
        y: annotPoint.y,
        text: annotText,
      });
      setNote(`已加批注并导出 ${r.path}（${Math.round(r.bytes / 1024)} KB）`);
      setAnnotText("");
    } catch (e) {
      setNote(`加批注失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [path, replacePage, annotPoint, annotText]);

  useEffect(() => {
    let cancelled = false;
    void invoke<PdfInfo>("pdf_info", { srcPath: path })
      .then((r) => {
        if (cancelled) return;
        setInfo(r);
        if (r.encrypted) {
          setError("该 PDF 使用 AES 加密，当前不支持编辑");
          return;
        }
        setPages(
          Array.from({ length: r.pages }, (_, i) => ({
            source: i + 1,
            // 绝对角度（后端写的也是绝对值）：原本就转过 90° 的页，
            // 再点一次「转 90°」应该是 180° 而不是 90°。
            rotation: r.rotations?.[i] ?? 0,
          })),
        );
      })
      .catch((e) => {
        if (!cancelled) setError(String(e instanceof Error ? e.message : e));
      });
    return () => {
      cancelled = true;
    };
  }, [path]);

  const toggle = useCallback((i: number) => {
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(i)) next.delete(i);
      else next.add(i);
      return next;
    });
  }, []);

  const move = useCallback((i: number, delta: number) => {
    setPages((ps) => {
      const j = i + delta;
      if (j < 0 || j >= ps.length) return ps;
      const next = ps.slice();
      [next[i], next[j]] = [next[j], next[i]];
      return next;
    });
    setSelected((s) => {
      const next = new Set(s);
      if (next.has(i)) {
        next.delete(i);
        next.add(i + delta);
      }
      return next;
    });
  }, []);

  const rotate = useCallback((i: number) => {
    setPages((ps) =>
      ps.map((p, idx) => (idx === i ? { ...p, rotation: (p.rotation + 90) % 360 } : p)),
    );
  }, []);

  const removeSelected = useCallback(() => {
    setPages((ps) => ps.filter((_, i) => !selected.has(i)));
    setSelected(new Set());
  }, [selected]);

  const insertBlank = useCallback(() => {
    setPages((ps) => [...ps, { source: null, rotation: 0 }]);
  }, []);

  const reset = useCallback(() => {
    if (!info) return;
    setPages(
      Array.from({ length: info.pages }, (_, i) => ({
        source: i + 1,
        rotation: info.rotations?.[i] ?? 0,
      })),
    );
    setSelected(new Set());
    setNote(null);
  }, [info]);

  /**
   * 页列表 → op 串。
   *
   * 顺序固定：keep → insertBlank（位置升序）→ rotate（最终页号）。
   * 空白页必须**从小到大**插：先插的小页占住前面的位置，后插的大页
   * 才不会因为页数变化而顶掉前面的页码。
   */
  const buildOps = useCallback((): PdfOp[] => {
    const originalCount = info?.pages ?? 0;
    const orig = pages
      .map((p) => p.source)
      .filter((v): v is number => v !== null);
    if (orig.length === 0) throw new Error("没有可保留的页面");
    const ops: PdfOp[] = [];
    // keep 同时表达「删页」与「重排」：它按传入顺序写页树。
    const identity =
      orig.length === originalCount && orig.every((v, i) => v === i + 1);
    if (!identity) {
      ops.push({ op: "keep", pages: orig });
    }
    // 空白页：最终位置 = 列表下标 + 1，按升序插入。
    const blanks = pages
      .map((p, i) => (p.source === null ? i + 1 : -1))
      .filter((v) => v > 0);
    for (const at of blanks) {
      ops.push({ op: "insertBlank", at, width: A4_W, height: A4_H });
    }
    // 旋转：按最终页号分组（一次 rotate op 只接受一个角度）。
    const byDeg = new Map<number, number[]>();
    pages.forEach((p, i) => {
      if (!p.rotation) return;
      const list = byDeg.get(p.rotation) ?? [];
      list.push(i + 1);
      byDeg.set(p.rotation, list);
    });
    for (const [degrees, list] of byDeg) {
      ops.push({ op: "rotate", pages: list, degrees });
    }
    return ops;
  }, [pages, info]);

  const doExport = useCallback(
    async (outPath: string | null) => {
      setBusy(true);
      setNote(null);
      try {
        const ops = buildOps();
        if (ops.length === 0) {
          setNote("没有改动，无需导出");
          return;
        }
        const r = await invoke<SaveResult>("pdf_ops", { srcPath: path, outPath, ops });
        setNote(`已导出 ${r.path}（${Math.round(r.bytes / 1024)} KB）`);
      } catch (e) {
        setNote(`导出失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [buildOps, path],
  );

  const doSaveAs = useCallback(async () => {
    setBusy(true);
    try {
      const out = await invoke<string | null>("image_save_dialog", {
        defaultName: defaultOutputName(name, "pdf"),
        filters: [["PDF", ["pdf"]]],
      });
      setBusy(false);
      if (!out) return;
      await doExport(out);
    } catch (e) {
      setBusy(false);
      setNote(`另存失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [name, doExport]);

  const reveal = useCallback(async () => {
    if (!note) return;
    const m = note.match(/已导出 (.+?)（/);
    if (m) await source.revealInExplorer?.(m[1]);
  }, [note, source]);

  /**
   * 文本层替换：只认 Tj 算子、整串精确相等。
   * 后端会先统计命中数，0 命中直接报错——不会静默产出一个内容没变的文件。
   */
  const doReplaceText = useCallback(async () => {
    if (!replaceFrom) {
      setNote("请填写要替换的原文");
      return;
    }
    setBusy(true);
    setNote(null);
    try {
      const r = await invoke<SaveResult>("pdf_replace_text", {
        srcPath: path,
        outPath: null,
        page: replacePage,
        from: replaceFrom,
        to: replaceTo,
      });
      setNote(
        `已替换 ${r.replaced ?? 0} 处并导出 ${r.path}（${Math.round(r.bytes / 1024)} KB）`,
      );
    } catch (e) {
      setNote(`替换失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [path, replacePage, replaceFrom, replaceTo]);

  /**
   * 按位置改字：盖住原字 + 写新字。
   *
   * 走的是"位置"而不是"内容流里的那串字节"—— 文本被拆成 TJ 数组、
   * 用自定义编码或子集字体时，字符串匹配会失配，位置匹配不会。
   */
  /**
   * 让扫描件可搜索：把这一页渲染成位图 → 调系统 OCR → 产出带隐形文字层的
   * 新 PDF。
   *
   * 渲染留在前端（Rust 侧没有渲染器，而 pdf.js 已经在渲染同一页了）；
   * 识别与写回在 Rust 侧（WinRT OCR + 隐形文字层）。
   */
  const doOcrPage = useCallback(async () => {
    if (!pdfDoc) {
      setNote("文档还没加载好");
      return;
    }
    setBusy(true);
    setNote(null);
    try {
      // 2 倍渲染：OCR 对小字更准，坐标换算也够细。
      const page = await pdfDoc.getPage(replacePage);
      const viewport = page.getViewport({ scale: 2 });
      const canvas = document.createElement("canvas");
      canvas.width = Math.floor(viewport.width);
      canvas.height = Math.floor(viewport.height);
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("拿不到画布上下文");
      ctx.fillStyle = "#ffffff";
      ctx.fillRect(0, 0, canvas.width, canvas.height);
      await page.render({ canvas, viewport } as never).promise;
      const dataUrl = canvas.toDataURL("image/png");
      const base64 = dataUrl.split(",")[1] ?? "";
      const r = await invoke<{
        path: string;
        words: number;
        text: string;
        language: string;
      }>("pdf_ocr_page", {
        srcPath: path,
        outPath: null,
        page: replacePage,
        pngBase64: base64,
      });
      setNote(
        `已识别 ${r.words} 个词（${r.language}），产物：${r.path}。打开它就能搜索、复制这些文字。`,
      );
      setOcrOut(r.path);
    } catch (e) {
      setNote(`OCR 失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [pdfDoc, replacePage, path]);

  const doReplaceAt = useCallback(async () => {
    if (!picked) {
      setNote("先在页面上点一句要改的字");
      return;
    }
    if (!newText.trim()) {
      setNote("新文字为空");
      return;
    }

    setBusy(true);
    setNote(null);
    try {
      const r = await invoke<SaveResult>("pdf_replace_text_at", {
        srcPath: path,
        outPath: null,
        req: {
          page: replacePage,
          rect: picked.rect,
          x: picked.x,
          y: picked.y,
          text: newText,
          fontSize: picked.size,
          // 真删除：把旧字的绘制算子从内容流里删掉（复制不出来）。
          removeText: picked.text,
        },
      });
      setNote(
        `${r.note ? r.note + "；" : ""}已替换并导出 ${r.path}（${Math.round(r.bytes / 1024)} KB）`,
      );
      setPicked(null);
    } catch (e) {
      setNote(`替换失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [path, replacePage, picked, newText]);

  /** 选图章图片：系统文件对话框 → 读字节 → 预览用的 object URL。 */
  const pickStamp = useCallback(async () => {
    try {
      const paths = await invoke<string[] | null>("image_open_dialog");
      if (!paths || paths.length === 0) return;
      const p = paths[0];
      const res = await source.readBytes(p);
      if (!res.ok) {
        setNote(`读取图章图片失败：${res.message}`);
        return;
      }
      // 复制成 ArrayBuffer 支撑的 Uint8Array：现代 TS 的 BlobPart 收紧到
      // ArrayBufferView<ArrayBuffer>，直接传 Uint8Array<ArrayBufferLike> 会被拒。
      const copy = new Uint8Array(res.bytes.byteLength);
      copy.set(res.bytes);
      const url = URL.createObjectURL(new Blob([copy]));
      setStampImage((old) => {
        if (old) URL.revokeObjectURL(old.url);
        return { path: p, url };
      });
      setTool("stamp");
      setNote("图章已选好：在页面上拖一个框把它放上去");
    } catch (e) {
      setNote(`选择图章图片失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [source]);

  /** 把待写入的标注写进 PDF（自由文字 / 高亮 / 墨迹 / 图章）。 */
  const doWriteAnnots = useCallback(async () => {
    if (annots.length === 0) {
      setNote("还没有标注");
      return;
    }
    setBusy(true);
    setNote(null);
    try {
      const r = await invoke<SaveResult>("pdf_add_annotations", {
        srcPath: path,
        outPath: null,
        page: replacePage,
        annotations: toWirePayload(annots),
      });
      setNote(
        `已写入 ${annots.length} 条标注并导出 ${r.path}（${Math.round(r.bytes / 1024)} KB）`,
      );
      setAnnots([]);
      setSelectedAnnot(null);
    } catch (e) {
      setNote(`写入标注失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [path, replacePage, annots]);

  const summary = useMemo(() => {
    const blanks = pages.filter((p) => p.source === null).length;
    const rotated = pages.filter((p) => p.rotation !== 0).length;
    const kept = pages.length - blanks;
    return `${kept} 页${blanks ? ` + ${blanks} 空白页` : ""}${
      rotated ? ` · ${rotated} 页已旋转` : ""
    }`;
  }, [pages]);

  return (
    <div className="ed-editor">
      <div className="ed-toolbar">
        <span className="ed-toolbar-name" title={path}>{name}</span>
        <div className="toolbar-spacer" />
        {/* 两个模式：管页面（增删排序）与在页面上画标注。 */}
        <div className="seg" role="group" aria-label="编辑模式">
          <button
            type="button"
            className={editMode === "pages" ? "is-active" : ""}
            onClick={() => setEditMode("pages")}
            aria-pressed={editMode === "pages"}
          >
            页面
          </button>
          <button
            type="button"
            className={editMode === "annot" ? "is-active" : ""}
            onClick={() => setEditMode("annot")}
            aria-pressed={editMode === "annot"}
          >
            标注
          </button>
          <button
            type="button"
            className={editMode === "text" ? "is-active" : ""}
            onClick={() => setEditMode("text")}
            aria-pressed={editMode === "text"}
          >
            改字
          </button>
        </div>
        <button
          type="button"
          className="tb-btn"
          onClick={() => setToolsOpen(true)}
          disabled={busy}
          title="文档工具（合并 / 裁剪 / 水印 / 页码 / 文档信息 / 密码）"
          aria-label="打开 PDF 文档工具"
        >
          <Wrench className="icon-sm" /> 文档工具
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doExport(null)}
          disabled={busy || !!error}
          title="导出为新文件"
          aria-label="导出 PDF"
        >
          <Save className="icon-sm" /> 导出
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doSaveAs()}
          disabled={busy || !!error}
          title="另存为…"
          aria-label="另存 PDF"
        >
          <FolderOpen className="icon-sm" /> 另存为
        </button>
        <button type="button" className="tb-btn" onClick={onClose} title="退出编辑" aria-label="退出编辑">
          退出
        </button>
      </div>

      {padOpen && (
        <SignaturePad
          onCancel={() => setPadOpen(false)}
          onConfirm={(data) => {
            setSignature({ base64: data.base64, url: data.url });
            setPadOpen(false);
            setTool("signature");
            setNote("签名已写好：在页面上拖一个框把它放上去");
          }}
        />
      )}

      <div className="ed-body">
        <div className="ed-panel">
          {error ? (
            <div className="preview-state-center">
              <div className="preview-state-error">
                <p>{error}</p>
                <small>{path}</small>
              </div>
            </div>
          ) : editMode === "text" ? (
            <PdfTextPicker
              doc={pdfDoc}
              page={replacePage}
              pickedId={picked?.id ?? null}
              onPick={(it) => {
                setPicked(it);
                setNewText(it.text);
              }}
            />
          ) : editMode === "annot" ? (
            <AnnotCanvas
              doc={pdfDoc}
              page={replacePage}
              tool={tool}
              annots={annots}
              selectedId={selectedAnnot}
              stampImage={stampImage}
              signature={signature}
              fieldName={fieldName}
              onAdd={(a) => setAnnots((list) => [...list, a])}
              onSelect={setSelectedAnnot}
            />
          ) : (
            <ol className="pdf-pages">
              {pages.map((p, i) => (
                <li
                  key={`${p.source ?? "blank"}-${i}`}
                  className={`pdf-page ${selected.has(i) ? "is-sel" : ""}`}
                >
                  <label className="pdf-page-check">
                    <input
                      type="checkbox"
                      checked={selected.has(i)}
                      onChange={() => toggle(i)}
                      aria-label={`选择第 ${i + 1} 页`}
                    />
                    <span className="pdf-page-label">
                      {p.source === null ? "空白页" : `第 ${p.source} 页`}
                      {p.rotation ? ` · ${p.rotation}°` : ""}
                    </span>
                  </label>
                  <span className="pdf-page-ops">
                    <button
                      type="button"
                      className="tb-btn"
                      onClick={() => move(i, -1)}
                      disabled={i === 0}
                      title="上移"
                      aria-label={`第 ${i + 1} 页上移`}
                    >
                      <ChevronUp className="icon-sm" />
                    </button>
                    <button
                      type="button"
                      className="tb-btn"
                      onClick={() => move(i, 1)}
                      disabled={i === pages.length - 1}
                      title="下移"
                      aria-label={`第 ${i + 1} 页下移`}
                    >
                      <ChevronDown className="icon-sm" />
                    </button>
                    <button
                      type="button"
                      className="tb-btn"
                      onClick={() => rotate(i)}
                      title="旋转 90°"
                      aria-label={`第 ${i + 1} 页旋转`}
                    >
                      <RotateCw className="icon-sm" />
                    </button>
                  </span>
                </li>
              ))}
            </ol>
          )}
        </div>

        <aside className="ed-side">
          {editMode === "text" && (
            <>
              <div className="sb-head">改字</div>
              <div className="ed-actions">
                <label className="ed-field">
                  <span>页码</span>
                  <input
                    type="number"
                    min={1}
                    max={info?.pages ?? 1}
                    value={replacePage}
                    onChange={(e) =>
                      setReplacePage(Math.max(1, Number(e.target.value) || 1))
                    }
                  />
                </label>
              </div>
              <div className="ed-status">
                在左边页面上**点一句字**（不用手打原文），然后在这里改它。
              </div>
              {picked ? (
                <>
                  <div className="ed-field">
                    <span>原文</span>
                    <div className="ed-picked">{picked.text}</div>
                  </div>
                  <label className="ed-field">
                    <span>新文字</span>
                    <input
                      value={newText}
                      onChange={(e) => setNewText(e.target.value)}
                      placeholder="改成…"
                    />
                  </label>
                  <div className="ed-status">字号 {Math.round(picked.size)}pt（取自原文字）</div>
                  <div className="ed-actions">
                    <button
                      type="button"
                      className="ed-btn"
                      disabled={busy || !!error}
                      onClick={() => void doReplaceAt()}
                    >
                      替换并导出
                    </button>
                  </div>
                </>
              ) : (
                <div className="ed-status">还没选中文字。</div>
              )}
              <div className="ed-status">
                改字 = **删掉旧字 + 写新字**：旧字不再存在于文件里（复制不出来）。
                中文暂不支持（要嵌字体），中文请用「标注」里的文字。
              </div>
            </>
          )}

          {editMode === "annot" && (
            <>
              <div className="sb-head">页面标注</div>
              <div className="ed-actions">
                <label className="ed-field">
                  <span>页码</span>
                  <input
                    type="number"
                    min={1}
                    max={info?.pages ?? 1}
                    value={replacePage}
                    onChange={(e) =>
                      setReplacePage(Math.max(1, Number(e.target.value) || 1))
                    }
                  />
                </label>
                <label className="ed-field">
                  <span>域名称</span>
                  <input
                    type="text"
                    value={fieldName}
                    placeholder="签名"
                    onChange={(e) => setFieldName(e.target.value)}
                  />
                </label>
              </div>
              <AnnotTools
                tool={tool}
                onTool={(t) => {
                  // 签名要先在签名板上写，写完才切工具。
                  if (t === "signature") {
                    setPadOpen(true);
                    return;
                  }
                  setTool(t);
                }}
                onPickStamp={() => void pickStamp()}
                stampName={stampImage ? (stampImage.path.split(/[\\/]/).pop() ?? null) : null}
                canDelete={!!selectedAnnot}
                onDelete={() => {
                  if (!selectedAnnot) return;
                  setAnnots((list) => list.filter((a) => a.id !== selectedAnnot));
                  setSelectedAnnot(null);
                }}
              />
              <div className="ed-status">
                文字：点一下页面直接写；高亮：拖一个框；墨迹：按住画；图章：先「选图章」再拖框放置；签名域：拖一个框留给别人签（不替人签）。选「选择」工具可点中已有标注后删除。
              </div>
              <div className="ed-status">
                已画 {annots.length} 条。写进 PDF 的是**标准注释**（FreeText / Highlight / Ink），
                Acrobat、福昕、Edge 都能看见并继续编辑。
              </div>
              <div className="ed-actions">
                <button
                  type="button"
                  className="ed-btn"
                  disabled={busy || !!error || annots.length === 0}
                  onClick={() => void doWriteAnnots()}
                >
                  写入并导出（{annots.length}）
                </button>
                <button
                  type="button"
                  className="ed-btn"
                  disabled={annots.length === 0}
                  onClick={() => {
                    setAnnots([]);
                    setSelectedAnnot(null);
                  }}
                >
                  清空
                </button>
              </div>
            </>
          )}

          <div className="sb-head">扫描件</div>
          <div className="ed-status">
            扫描件（图片型 PDF）没有文字层，既搜不到也复制不了。用系统自带的 OCR
            给这一页补一层**看不见的文字**（产物是新文件，原文件不动）。
          </div>
          <div className="ed-actions">
            <button
              type="button"
              className="ed-btn"
              disabled={busy || !!error || !pdfDoc}
              onClick={() => void doOcrPage()}
            >
              让第 {replacePage} 页可搜索
            </button>
          </div>
          {ocrOut && (
            <div className="ed-actions">
              <button
                type="button"
                className="ed-btn"
                onClick={() => void source.openWithSystem?.(ocrOut)}
              >
                打开 OCR 产物
              </button>
              <button
                type="button"
                className="ed-btn"
                onClick={() => void source.revealInExplorer?.(ocrOut)}
              >
                在资源管理器中显示
              </button>
            </div>
          )}

          <div className="sb-head">页面</div>
          <div className="ed-status">{summary}</div>
          <div className="ed-actions">
            <button
              type="button"
              className="ed-btn"
              onClick={removeSelected}
              disabled={selected.size === 0}
            >
              删除选中（{selected.size}）
            </button>
            <button type="button" className="ed-btn" onClick={insertBlank}>
              <FilePlus2 className="icon-sm" /> 末尾插空白页
            </button>
            <button type="button" className="ed-btn" onClick={reset} disabled={!info}>
              还原
            </button>
          </div>
          <div className="ed-status">
            导出为新文件，原 PDF 不会被改动。
          </div>

          <div className="sb-head">文本替换</div>
          <div className="ed-actions">
            <label className="ed-field">
              <span>页码</span>
              <input
                type="number"
                min={1}
                max={info?.pages ?? 1}
                value={replacePage}
                onChange={(e) =>
                  setReplacePage(Math.max(1, Number(e.target.value) || 1))
                }
              />
            </label>
            <label className="ed-field">
              <span>原文</span>
              <input
                value={replaceFrom}
                placeholder="整串匹配，如「旧标题」"
                onChange={(e) => setReplaceFrom(e.target.value)}
              />
            </label>
            <label className="ed-field">
              <span>新文</span>
              <input
                value={replaceTo}
                placeholder="留空即删除"
                onChange={(e) => setReplaceTo(e.target.value)}
              />
            </label>
            <button
              type="button"
              className="ed-btn"
              disabled={busy || !!error || !replaceFrom}
              onClick={() => void doReplaceText()}
            >
              替换并导出
            </button>
          </div>
          <div className="ed-status">
            只处理文本层里的整串文字（扫描件与 TJ 数组排版会明确报「未找到」）。
          </div>

          {/* 本页文字清单：点一下就填进「原文」。
              之前只能手打整串原文 —— 实际没人能一字不差地打完一个长句，
              于是这个功能等于没有。 */}
          {textItems.length > 0 && (
            <>
              <div className="sb-head">本页文字（点一下填入原文）</div>
              <div className="pdf-text-items">
                {textItems.map((t, i) => (
                  <button
                    key={`${i}-${t.slice(0, 12)}`}
                    type="button"
                    className={replaceFrom === t ? "pdf-text-item is-active" : "pdf-text-item"}
                    onClick={() => setReplaceFrom(t)}
                    title={t}
                  >
                    {t}
                  </button>
                ))}
              </div>
            </>
          )}

          {/* 批注：点页面选位置 + 写内容 → 写进 PDF 的 /Annots。 */}
          <div className="sb-head">便签批注</div>
          <div className="ed-actions">
            <canvas
              ref={canvasRef}
              className="pdf-annot-canvas"
              onClick={onCanvasClick}
              title="点一下要放批注的位置"
            />
            <div className="ed-status">
              {annotPoint
                ? `位置已选：x=${Math.round(annotPoint.x)} y=${Math.round(annotPoint.y)}（PDF 坐标）`
                : "在上面的页面预览里点一下，选批注的落点。"}
            </div>
            <label className="ed-field">
              <span>批注</span>
              <input
                value={annotText}
                placeholder="要写的话"
                onChange={(e) => setAnnotText(e.target.value)}
              />
            </label>
            <button
              type="button"
              className="ed-btn"
              disabled={busy || !!error || !annotPoint || !annotText.trim()}
              onClick={() => void doAnnotate()}
            >
              <MessageSquarePlus className="icon-sm" /> 加批注并导出
            </button>
          </div>
          <div className="ed-status">
            批注写进 PDF 的注释层（Acrobat / 福昕 / Edge 都能看），不是画在图片上的假字。
          </div>
          {note && (
            <div className="ed-note">
              {note}
              {note.startsWith("已导出") && (
                <button type="button" className="img-link" onClick={() => void reveal()}>
                  在资源管理器中显示
                </button>
              )}
            </div>
          )}
        </aside>
      </div>
      {toolsOpen && (
        <PdfToolsDialog
          path={path}
          name={name}
          source={source}
          onClose={() => setToolsOpen(false)}
        />
      )}
    </div>
  );
}

function defaultOutputName(name: string, ext: string): string {
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  return `${stem}.edited.${ext}`;
}
