// PPTX 编辑器（重量模式专属）——**演示文稿就地编辑**。
//
// 为什么能编辑：项目里已经装了 `pptx-browser`，而它不只带渲染器
// （PptxRenderer），还带一个**写回器**（PptxWriter：改形状文字、加文本框、
// 加图、复制/删除/重排页、改页背景、改主题色、写备注、save()）。之前只用了
// 渲染面 —— 于是 PPT 只能看不能改，类型声明里也只有渲染面。
//
// 这里**不引新依赖**（六步决策阶梯第 4 级：现有依赖优先）：能改的 API 已经
// 在磁盘上，缺的只是把它接起来 + 补上类型声明。
//
// 显示与写回的关系：PptxWriter 改的是 OOXML 文件本体；渲染器读的是另一份
// 副本，所以每次改动后要把 writer 的产物重新灌回渲染器（`renderer.load` 可
// 以原地重载），画布才与改动一致。
//
// 原文件永不覆盖：产物走 Rust 的 `write_bytes`（带 safe_output_path 守卫），
// 默认落 `<原名>.edited.pptx`。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ChevronLeft,
  ChevronRight,
  Copy,
  FolderOpen,
  Plus,
  Save,
  StickyNote,
  Trash2,
  Type,
  ZoomIn,
  ZoomOut,
} from "lucide-react";
import { PptxRenderer, PptxWriter } from "pptx-browser";
import { encodeBase64, editedName } from "./office";
import type { FileSource } from "@hermix/preview";

interface SaveResult {
  path: string;
  bytes: number;
}

interface ShapeInfo {
  id: string;
  name: string;
  type: string;
}

const MIN_WIDTH = 480;
const MAX_WIDTH = 1920;
/** 幻灯片尺寸（EMU）—— 加文本框时按比例定位。 */
const SLIDE_CX = 9144000;
const SLIDE_CY = 5143500;

export function PptxEditor({
  path,
  name,
  source,
  onPreview,
}: {
  path: string;
  name: string;
  source: FileSource;
  /** 切回只读预览（内嵌在预览区时提供）。 */
  onPreview?: () => void;
}) {
  const rendererRef = useRef<PptxRenderer | null>(null);
  const writerRef = useRef<PptxWriter | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);

  const [slideCount, setSlideCount] = useState(0);
  const [slide, setSlide] = useState(0);
  const [width, setWidth] = useState(1024);
  const [shapes, setShapes] = useState<ShapeInfo[]>([]);
  const [picked, setPicked] = useState<string | null>(null);
  const [text, setText] = useState("");
  const [notes, setNotes] = useState("");
  const [dirty, setDirty] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // 打开文件：渲染器负责画，写回器负责改。
  useEffect(() => {
    let cancelled = false;
    setError(null);
    setSlideCount(0);
    setDirty(0);
    const renderer = new PptxRenderer();
    rendererRef.current = renderer;
    void (async () => {
      try {
        const res = await source.readBytes(path);
        if (!res.ok) throw new Error(res.message);
        const bytes = new Uint8Array(res.bytes);
        await renderer.load(bytes);
        const writer = await PptxWriter.fromBytes(bytes);
        if (cancelled) return;
        writerRef.current = writer;
        setSlideCount(renderer.slideCount);
        setSlide(0);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
      renderer.destroy();
      rendererRef.current = null;
      writerRef.current = null;
    };
  }, [path, source]);

  /** 画当前页。 */
  const draw = useCallback(async () => {
    const renderer = rendererRef.current;
    const canvas = canvasRef.current;
    if (!renderer || !canvas || slideCount === 0) return;
    try {
      await renderer.renderSlide(slide, canvas, width);
    } catch {
      /* 单页渲染失败保留上一帧，不整块崩 */
    }
  }, [slide, width, slideCount]);

  useEffect(() => {
    void draw();
  }, [draw]);

  /** 改动写回器 → 重新灌进渲染器 → 画布跟上。 */
  const commit = useCallback(
    async (mutate: (w: PptxWriter) => void) => {
      const writer = writerRef.current;
      const renderer = rendererRef.current;
      if (!writer || !renderer) return;
      try {
        mutate(writer);
        setDirty((n) => n + 1);
        const bytes = await writer.save();
        await renderer.load(bytes);
        setSlideCount(renderer.slideCount);
        setShapes(writer.listShapes(Math.min(slide, writer.getSlideCount() - 1)));
      } catch (e) {
        setNote(`操作失败：${e instanceof Error ? e.message : String(e)}`);
      }
    },
    [slide],
  );

  // 切页时刷新形状列表与备注。
  useEffect(() => {
    const writer = writerRef.current;
    if (!writer || slideCount === 0) return;
    const idx = Math.min(slide, writer.getSlideCount() - 1);
    try {
      setShapes(writer.listShapes(idx));
      setPicked(null);
      setText("");
    } catch {
      setShapes([]);
    }
  }, [slide, slideCount]);

  const pickShape = (shape: ShapeInfo) => {
    const writer = writerRef.current;
    if (!writer) return;
    const idx = Math.min(slide, writer.getSlideCount() - 1);
    try {
      setPicked(shape.name);
      setText(writer.getShapeText(idx, shape.name));
    } catch {
      setPicked(shape.name);
      setText("");
    }
  };

  const applyText = () => {
    if (!picked) return;
    void commit((w) => {
      const idx = Math.min(slide, w.getSlideCount() - 1);
      w.setShapeText(idx, picked, text);
    });
  };

  const addTextBox = () => {
    const content = text.trim() || "新文本";
    void commit((w) => {
      const idx = Math.min(slide, w.getSlideCount() - 1);
      w.addTextBox(idx, content, {
        x: Math.round(SLIDE_CX * 0.25),
        y: Math.round(SLIDE_CY * 0.35),
        w: Math.round(SLIDE_CX * 0.5),
        h: Math.round(SLIDE_CY * 0.15),
        fontSize: 2400,
        color: "000000",
      });
    });
  };

  const duplicateSlide = () => {
    void commit((w) => {
      const idx = Math.min(slide, w.getSlideCount() - 1);
      w.duplicateSlide(idx, idx + 1);
    });
    setSlide((s) => s + 1);
  };

  const removeSlide = () => {
    if (slideCount <= 1) {
      setNote("只剩一页，删不得");
      return;
    }
    void commit((w) => {
      const idx = Math.min(slide, w.getSlideCount() - 1);
      w.removeSlide(idx);
    });
    setSlide((s) => Math.max(0, s - 1));
  };

  const moveSlide = (delta: number) => {
    void commit((w) => {
      const n = w.getSlideCount();
      const order = Array.from({ length: n }, (_, i) => i);
      const idx = Math.min(slide, n - 1);
      const to = Math.min(n - 1, Math.max(0, idx + delta));
      if (to === idx) return;
      const [item] = order.splice(idx, 1);
      order.splice(to, 0, item);
      w.reorderSlides(order);
    });
    setSlide((s) => Math.max(0, Math.min(slideCount - 1, s + delta)));
  };

  const setBackground = (hex: string) => {
    void commit((w) => {
      const idx = Math.min(slide, w.getSlideCount() - 1);
      w.setSlideBackground(idx, hex.replace("#", ""));
    });
  };

  const doWrite = useCallback(
    async (outPath: string | null) => {
      const writer = writerRef.current;
      if (!writer) return;
      setBusy(true);
      setNote(null);
      try {
        const out = await writer.save();
        const res = await invoke<SaveResult>("write_bytes", {
          srcPath: path,
          outPath,
          dataBase64: encodeBase64(new Uint8Array(out)),
        });
        setNote(`已导出 ${res.path}（${Math.round(res.bytes / 1024)} KB）`);
      } catch (e) {
        setNote(`导出失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [path],
  );

  const doSaveAs = useCallback(async () => {
    setBusy(true);
    try {
      const out = await invoke<string | null>("image_save_dialog", {
        defaultName: editedName(name, "pptx"),
        filters: [["PowerPoint", ["pptx"]]],
      });
      setBusy(false);
      if (!out) return;
      await doWrite(out);
    } catch (e) {
      setBusy(false);
      setNote(`另存失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [name, doWrite]);

  const reveal = useCallback(() => {
    if (!note) return;
    const m = note.match(/已导出 (.+?)（/);
    if (m) void invoke("viewer_reveal_in_explorer", { path: m[1] });
  }, [note]);

  const slideTitle = useMemo(() => {
    const shape = shapes.find((s) => s.type === "sp");
    return shape?.name ?? "";
  }, [shapes]);

  return (
    <div className="ed-editor">
      <div className="ed-toolbar">
        <span className="ed-toolbar-name" title={path}>
          {name}
        </span>
        {dirty > 0 && <span className="ed-dirty">已改 {dirty} 处</span>}
        <div className="toolbar-spacer" />
        <button
          type="button"
          className="tb-btn"
          onClick={() => setSlide((s) => Math.max(0, s - 1))}
          disabled={slide <= 0}
          title="上一页"
          aria-label="上一页"
        >
          <ChevronLeft className="icon-sm" />
        </button>
        <span className="px-page">
          {slideCount ? `${slide + 1} / ${slideCount}` : "载入中…"}
        </span>
        <button
          type="button"
          className="tb-btn"
          onClick={() => setSlide((s) => Math.min(slideCount - 1, s + 1))}
          disabled={slideCount === 0 || slide >= slideCount - 1}
          title="下一页"
          aria-label="下一页"
        >
          <ChevronRight className="icon-sm" />
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => setWidth((w) => Math.max(MIN_WIDTH, w - 160))}
          title="缩小"
          aria-label="缩小"
        >
          <ZoomOut className="icon-sm" />
        </button>
        <span className="px-page">{Math.round((width / 1024) * 100)}%</span>
        <button
          type="button"
          className="tb-btn"
          onClick={() => setWidth((w) => Math.min(MAX_WIDTH, w + 160))}
          title="放大"
          aria-label="放大"
        >
          <ZoomIn className="icon-sm" />
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void commit((w) => w.setSlideNotes(Math.min(slide, w.getSlideCount() - 1), notes || "（备注）"))}
          disabled={slideCount === 0}
          title="把右栏的备注写进这一页的备注栏"
          aria-label="写入备注"
        >
          <StickyNote className="icon-sm" /> 写备注
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={duplicateSlide}
          disabled={slideCount === 0}
          title="复制当前页"
          aria-label="复制当前页"
        >
          <Copy className="icon-sm" /> 复制页
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={removeSlide}
          disabled={slideCount <= 1}
          title="删除当前页"
          aria-label="删除当前页"
        >
          <Trash2 className="icon-sm" /> 删页
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doWrite(null)}
          disabled={busy || dirty === 0 || !!error}
          title="导出为新文件"
          aria-label="导出演示文稿"
        >
          <Save className="icon-sm" /> 导出
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doSaveAs()}
          disabled={busy || dirty === 0 || !!error}
          title="另存为…"
          aria-label="另存演示文稿"
        >
          <FolderOpen className="icon-sm" /> 另存为
        </button>
        {onPreview && (
          <button
            type="button"
            className="tb-btn"
            onClick={onPreview}
            title="切回只读预览（改动不会丢，只是不再显示编辑面）"
            aria-label="切回只读预览"
          >
            预览
          </button>
        )}
      </div>

      <div className="ed-body">
        <div className="ed-panel px-panel">
          {error ? (
            <div className="preview-state-center">
              <div className="preview-state-error">
                <p>{error}</p>
                <small>{path}</small>
              </div>
            </div>
          ) : (
            <canvas ref={canvasRef} className="px-canvas" />
          )}
        </div>

        <aside className="ed-side">
          <div className="sb-head">形状</div>
          <div className="px-shape-list">
            {shapes.map((s) => (
              <button
                key={`${s.id}-${s.name}`}
                type="button"
                className={picked === s.name ? "px-shape is-active" : "px-shape"}
                onClick={() => pickShape(s)}
                title={s.name}
              >
                <Type className="icon-sm" />
                <span>{s.name || "(未命名)"}</span>
              </button>
            ))}
            {shapes.length === 0 && <div className="px-hint">这一页没有可选形状</div>}
          </div>

          <div className="sb-head">文字</div>
          <textarea
            className="px-text"
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="选中形状后在这里改字；也可以直接写新文本框的内容"
            rows={5}
          />
          <div className="px-actions">
            <button
              type="button"
              className="tb-btn"
              onClick={applyText}
              disabled={!picked}
              title="把上面的文字写回所选形状"
            >
              应用到形状
            </button>
            <button
              type="button"
              className="tb-btn"
              onClick={addTextBox}
              disabled={slideCount === 0}
              title="在当前页新增一个文本框"
            >
              <Plus className="icon-sm" /> 新增文本框
            </button>
          </div>

          <div className="sb-head">备注</div>
          <textarea
            className="px-text"
            value={notes}
            onChange={(e) => setNotes(e.target.value)}
            placeholder="这一页的演讲备注"
            rows={3}
          />

          <div className="sb-head">页背景</div>
          <div className="px-actions">
            {["FFFFFF", "F2F2F2", "FFF2CC", "E2EFDA", "DDEBF7"].map((hex) => (
              <button
                key={hex}
                type="button"
                className="px-color"
                style={{ background: `#${hex}` } as CSSProperties}
                onClick={() => setBackground(hex)}
                title={`背景 #${hex}`}
                aria-label={`背景色 ${hex}`}
              />
            ))}
          </div>

          <div className="sb-head">顺序</div>
          <div className="px-actions">
            <button
              type="button"
              className="tb-btn"
              onClick={() => moveSlide(-1)}
              disabled={slide <= 0}
            >
              前移
            </button>
            <button
              type="button"
              className="tb-btn"
              onClick={() => moveSlide(1)}
              disabled={slideCount === 0 || slide >= slideCount - 1}
            >
              后移
            </button>
          </div>

          <div className="ed-status">
            {slideTitle ? `当前页首个形状：${slideTitle}` : "选中左侧形状即可改它的文字。"}
          </div>
          <div className="ed-status">原文件不会被改动，导出为新文件。</div>
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
    </div>
  );
}
