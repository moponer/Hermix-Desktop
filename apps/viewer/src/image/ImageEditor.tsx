// 图片编辑器（重量模式专属）。
//
// 形态：工具条 + 画布 + 右侧参数区，**不用模态**——每调一个参数弹一次窗，
// 一张图要开八次窗，交互被切碎。参数区跟着当前工具变。
//
// 职责分工：预览在前端 Canvas（要 60fps），导出在 Rust（权威）。
// 两边共用同一份 op 模型与公式约定（见 imageOps.ts 顶部注释）。
//
// 坐标系（本文件最容易错的地方，钉死三条）：
//   1. op 里的坐标一律是**原图像素空间**（Rust 侧对解码后的原图套用 op）。
//   2. 预览画布 = 状态尺寸 × scale，scale = previewScale(原图宽高, MAX_SIDE)。
//      浏览器若解不了原格式（ico/tiff）会用 Rust 转出的**降采样**图当画面，
//      但几何仍按原图尺寸算——所以几何永远是对的，只是画面略糊。
//   3. 指针事件给的是 **CSS 像素**，画布的 backing store 是**设备像素**，
//      两者还要再除以 scale 才落到原图空间。三步换算缺一不可。
//
// 原图只读：所有产物都落到新文件（Rust 侧硬校验），这里不做覆盖动作。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Crop, FlipHorizontal, FlipVertical, RotateCw, RotateCcw, Undo2, Redo2,
  Save, FolderOpen, SlidersHorizontal, Type, Images, X,
} from "lucide-react";
import type { FileSource } from "@hermix/preview";
import {
  renderOps,
  previewScale,
  stateSize,
  drawTextLayers,
  readTokenColor,
  EDITABLE_FORMATS,
  type ImageOp,
  type TextLayer,
} from "./imageOps";

const MAX_SIDE = 1400;

/** 可编辑的栅格格式（SVG 是矢量，`image` 解不了，不进编辑器）。 */
const FORMATS = EDITABLE_FORMATS;

/**
 * 浏览器画布上限（Chrome：单边 16384、总面积 268M 像素）。
 * 超过就没有第二条渲染路径了——只能明确报错，不能默默导出一张空图。
 */
const CANVAS_MAX_SIDE = 16384;
const CANVAS_MAX_AREA = 268_000_000;

/** 文字图层 id 计数器（模块级：跨实例唯一）。 */
let nextTextId = 1;

type Tool = "none" | "crop" | "text";

interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

interface ExportResult {
  path: string;
  bytes: number;
  width: number;
  height: number;
}

/** 参数类 op：同类型只留一个，重复设置即替换（滑杆拖动不该堆几百个 op）。 */
const PARAM_OPS = new Set([
  "grayscale", "brighten", "contrast", "saturate", "blur", "mosaic", "resize",
]);

function isParamOp(op: ImageOp): boolean {
  return PARAM_OPS.has(op.op);
}

export function ImageEditor({
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
  const [img, setImg] = useState<HTMLImageElement | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  /** 原图尺寸（权威）：几何换算的基准，**不随预览降采样变化**。 */
  const [orig, setOrig] = useState<{ w: number; h: number } | null>(null);
  const [ops, setOps] = useState<ImageOp[]>([]);
  const [past, setPast] = useState<ImageOp[][]>([]);
  const [future, setFuture] = useState<ImageOp[][]>([]);
  const [tool, setTool] = useState<Tool>("none");
  const [cropRect, setCropRect] = useState<Rect | null>(null);
  /** 文字图层：独立于 op 链（见 imageOps.ts 的说明）。 */
  const [texts, setTexts] = useState<TextLayer[]>([]);
  const [activeText, setActiveText] = useState<number | null>(null);
  /** 拼接用的其它图片（当前图永远是第一张）。 */
  const [montagePaths, setMontagePaths] = useState<string[]>([]);
  const [format, setFormat] = useState(() => defaultFormat(name));
  const [quality, setQuality] = useState(90);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const dragStart = useRef<{ x: number; y: number } | null>(null);

  // ── 载入源图 ────────────────────────────────────────────────
  // 两步：先问 Rust 原图真身尺寸（几何基准），再取像素画出来。
  // 浏览器解不了的格式（ico / tiff）走 Rust 转 PNG 再显示。
  useEffect(() => {
    let url: string | null = null;
    let cancelled = false;
    const load = async () => {
      const info = await invoke<{ width: number; height: number }>("image_info", { path })
        .catch(() => null);
      if (cancelled) return;
      if (info) setOrig({ w: info.width, h: info.height });

      const r = await source.readBytes(path);
      if (cancelled) return;
      if (!r.ok) {
        setLoadError(r.message);
        return;
      }
      const blob = new Blob([r.bytes as BlobPart], { type: mimeOf(name) });
      url = URL.createObjectURL(blob);
      const im = new Image();
      im.onload = () => {
        if (cancelled) return;
        setImg(im);
        // 兜底：image_info 失败时用解码出来的尺寸当基准。
        setOrig((cur) => cur ?? { w: im.naturalWidth, h: im.naturalHeight });
      };
      im.onerror = () => {
        // 浏览器解不了 → 让 Rust 转成 PNG（降采样版：几何仍按原图算）
        void invoke<{ pngBase64: string }>("image_rasterize", { path, maxSide: MAX_SIDE })
          .then((out) => {
            if (cancelled) return;
            const im2 = new Image();
            im2.onload = () => {
              if (cancelled) return;
              setImg(im2);
              setOrig((cur) => cur ?? { w: im2.naturalWidth, h: im2.naturalHeight });
            };
            im2.src = `data:image/png;base64,${out.pngBase64}`;
          })
          .catch((e) => {
            if (!cancelled) setLoadError(String(e instanceof Error ? e.message : e));
          });
      };
      im.src = url;
    };
    void load();
    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [path, name, source]);

  // ── 预览渲染 ────────────────────────────────────────────────
  const srcW = orig?.w ?? 0;
  const srcH = orig?.h ?? 0;
  const state = useMemo(() => stateSize(srcW, srcH, ops), [srcW, srcH, ops]);
  const scale = useMemo(() => previewScale(srcW, srcH, MAX_SIDE), [srcW, srcH]);

  useEffect(() => {
    if (!img) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const out = renderOps(img, srcW, srcH, ops, MAX_SIDE);
    canvas.width = out.width;
    canvas.height = out.height;
    const ctx = out.getContext("2d");
    if (!ctx) return;
    ctx.drawImage(out, 0, 0);
    // 文字图层画在 op 结果之上（原图空间 × scale）。
    drawTextLayers(ctx, texts, scale);
  }, [img, ops, srcW, srcH, texts, scale]);

  // ── op 变更（带撤销栈）─────────────────────────────────────
  // 注意：不要在 setState 的 updater 里再 setState——updater 必须是纯函数，
  // StrictMode 下会被调用两次，副作用会翻倍。这里用闭包里的 ops 直接算。
  const pushOps = useCallback(
    (next: ImageOp[]) => {
      setPast((p) => [...p, ops]);
      setFuture([]);
      setOps(next);
    },
    [ops],
  );

  const undo = useCallback(() => {
    if (past.length === 0) return;
    const prev = past[past.length - 1];
    setPast(past.slice(0, -1));
    setFuture([ops, ...future]);
    setOps(prev);
  }, [past, future, ops]);

  const redo = useCallback(() => {
    if (future.length === 0) return;
    const [next, ...rest] = future;
    setPast([...past, ops]);
    setFuture(rest);
    setOps(next);
  }, [past, future, ops]);

  /** 参数类：同类型替换；动作类：追加。 */
  const applyOp = useCallback(
    (op: ImageOp) => {
      if (isParamOp(op)) {
        pushOps([...ops.filter((o) => o.op !== op.op), op]);
      } else {
        pushOps([...ops, op]);
      }
    },
    [ops, pushOps],
  );

  const reset = useCallback(() => {
    pushOps([]);
    setCropRect(null);
    setTool("none");
    setTexts([]);
    setActiveText(null);
  }, [pushOps]);

  /** 改一个文字图层的字段。 */
  const updateText = useCallback((id: number, patch: Partial<TextLayer>) => {
    setTexts((ts) => ts.map((t) => (t.id === id ? { ...t, ...patch } : t)));
  }, []);

  // ── 裁剪交互（CSS 像素 → 画布像素 → 原图空间）──────────────
  /** CSS 像素 → 原图空间的换算系数：画布设备像素/CSS 像素 ÷ preview scale。 */
  const cssToState = useCallback(
    (cssLen: number, canvasEl: HTMLCanvasElement): number => {
      const rect = canvasEl.getBoundingClientRect();
      if (rect.width <= 0 || canvasEl.width <= 0) return 0;
      const deviceRatio = canvasEl.width / rect.width;
      return (cssLen * deviceRatio) / scale;
    },
    [scale],
  );

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      if (tool !== "crop") return;
      const rect = e.currentTarget.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      dragStart.current = { x, y };
      setCropRect({ x, y, w: 0, h: 0 });
    },
    [tool],
  );

  const onPointerMove = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      if (tool !== "crop" || !dragStart.current) return;
      const rect = e.currentTarget.getBoundingClientRect();
      const x = e.clientX - rect.left;
      const y = e.clientY - rect.top;
      const sx = dragStart.current.x;
      const sy = dragStart.current.y;
      setCropRect({
        x: Math.min(sx, x),
        y: Math.min(sy, y),
        w: Math.abs(x - sx),
        h: Math.abs(y - sy),
      });
    },
    [tool],
  );

  const onPointerUp = useCallback(
    (e: React.PointerEvent<HTMLCanvasElement>) => {
      dragStart.current = null;
      if (tool === "text") {
        // 文字模式：点哪儿放哪儿（左上角锚点）。
        const canvas = e.currentTarget;
        const rect = canvas.getBoundingClientRect();
        const x = Math.max(0, Math.round(cssToState(e.clientX - rect.left, canvas)));
        const y = Math.max(0, Math.round(cssToState(e.clientY - rect.top, canvas)));
        const id = nextTextId++;
        setTexts((ts) => [
          ...ts,
          {
            id,
            text: "文字",
            x,
            y,
            size: Math.max(8, Math.round(state.h * 0.06)),
            color: readTokenColor("--text-primary"),
          },
        ]);
        setActiveText(id);
        setTool("none");
        return;
      }
      if (tool !== "crop" || !cropRect) return;
      if (cropRect.w < 4 || cropRect.h < 4) {
        setCropRect(null);
        return;
      }
      const canvas = e.currentTarget;
      const x = Math.max(0, Math.round(cssToState(cropRect.x, canvas)));
      const y = Math.max(0, Math.round(cssToState(cropRect.y, canvas)));
      const w = Math.max(1, Math.round(cssToState(cropRect.w, canvas)));
      const h = Math.max(1, Math.round(cssToState(cropRect.h, canvas)));
      pushOps([...ops, { op: "crop", x, y, w, h }]);
      setCropRect(null);
      setTool("none");
    },
    [tool, cropRect, cssToState, ops, pushOps, state.h],
  );

  // ── 导出 ───────────────────────────────────────────────────
  /**
   * 带文字图层时的合成：整幅在浏览器里按**原图分辨率**画一遍，
   * 再把 PNG 交给 Rust 转目标格式落盘（Rust 直接写 PNG 会让
   * 「.jpg 文件里装着 PNG」）。
   */
  const composeFull = useCallback(async (): Promise<Blob> => {
    if (!img) throw new Error("图片尚未载入");
    if (state.w > CANVAS_MAX_SIDE || state.h > CANVAS_MAX_SIDE) {
      throw new Error(
        `合成尺寸 ${state.w}×${state.h} 超过浏览器画布上限（${CANVAS_MAX_SIDE}），带文字图层时无法导出`,
      );
    }
    if (state.w * state.h > CANVAS_MAX_AREA) {
      throw new Error("合成后的像素总量超过浏览器画布上限，无法导出");
    }
    const out = renderOps(img, srcW, srcH, ops, Number.MAX_SAFE_INTEGER);
    const ctx = out.getContext("2d");
    if (!ctx) throw new Error("画布不可用");
    drawTextLayers(ctx, texts, 1);
    const blob = await new Promise<Blob | null>((res) => out.toBlob(res, "image/png"));
    if (!blob) throw new Error("浏览器合成失败");
    return blob;
  }, [img, srcW, srcH, ops, texts, state.w, state.h]);

  const doExport = useCallback(
    async (outPath: string | null) => {
      setBusy(true);
      setNote(null);
      try {
        if (texts.length > 0) {
          const blob = await composeFull();
          const pngBase64 = await blobToBase64(blob);
          const r = await invoke<ExportResult>("image_write_composite", {
            srcPath: path,
            outPath,
            pngBase64,
            format,
            quality,
          });
          setNote(`已导出 ${r.path}（${r.width}×${r.height}，${Math.round(r.bytes / 1024)} KB）`);
          return;
        }
        const r = await invoke<ExportResult>("image_apply", {
          path,
          ops,
          outPath,
          format,
          quality,
        });
        setNote(`已导出 ${r.path}（${r.width}×${r.height}，${Math.round(r.bytes / 1024)} KB）`);
      } catch (e) {
        setNote(`导出失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [path, ops, format, quality, texts, composeFull],
  );

  // ── 拼接 ───────────────────────────────────────────────────
  const pickMontage = useCallback(async () => {
    try {
      const picked = await invoke<string[] | null>("image_open_dialog");
      if (!picked || picked.length === 0) return;
      setMontagePaths((ps) => [...ps, ...picked]);
    } catch (e) {
      setNote(`选择失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, []);

  const doMontage = useCallback(async () => {
    setBusy(true);
    setNote(null);
    try {
      const r = await invoke<ExportResult>("image_montage", {
        paths: [path, ...montagePaths],
        columns: 0,
        spacing: 8,
        outPath: null,
        format,
        quality,
      });
      setNote(
        `已拼接 ${montagePaths.length + 1} 张 → ${r.path}（${r.width}×${r.height}，${Math.round(r.bytes / 1024)} KB）`,
      );
    } catch (e) {
      setNote(`拼接失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setBusy(false);
    }
  }, [path, montagePaths, format, quality]);

  const doSaveAs = useCallback(async () => {
    setBusy(true);
    try {
      const out = await invoke<string | null>("image_save_dialog", {
        defaultName: defaultOutputName(name, format),
        filters: [[format.toUpperCase(), extList(format)]],
      });
      setBusy(false);
      if (!out) return;
      await doExport(out);
    } catch (e) {
      setBusy(false);
      setNote(`另存失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [name, format, doExport]);

  const reveal = useCallback(async () => {
    if (!note) return;
    const m = note.match(/已导出 (.+?)（/);
    if (m) await source.revealInExplorer?.(m[1]);
  }, [note, source]);

  // ── 参数区当前值 ───────────────────────────────────────────
  const param = useCallback(
    (op: ImageOp["op"]): ImageOp | undefined => ops.find((o) => o.op === op),
    [ops],
  );

  const brighten = param("brighten") as Extract<ImageOp, { op: "brighten" }> | undefined;
  const contrast = param("contrast") as Extract<ImageOp, { op: "contrast" }> | undefined;
  const saturate = param("saturate") as Extract<ImageOp, { op: "saturate" }> | undefined;
  const blur = param("blur") as Extract<ImageOp, { op: "blur" }> | undefined;
  const mosaic = param("mosaic") as Extract<ImageOp, { op: "mosaic" }> | undefined;
  const resize = param("resize") as Extract<ImageOp, { op: "resize" }> | undefined;
  const isGray = !!param("grayscale");
  const activeLayer = useMemo(
    () => texts.find((t) => t.id === activeText),
    [texts, activeText],
  );

  return (
    <div className="img-editor">
      <div className="img-toolbar">
        <button
          type="button"
          className={`tb-btn ${tool === "crop" ? "is-active" : ""}`}
          onClick={() => setTool(tool === "crop" ? "none" : "crop")}
          title="裁剪（拖动选框）"
          aria-label="裁剪"
        >
          <Crop />
        </button>
        <button type="button" className="tb-btn" onClick={() => applyOp({ op: "rotate", degrees: 90 })} title="向右旋转 90°" aria-label="向右旋转">
          <RotateCw />
        </button>
        <button type="button" className="tb-btn" onClick={() => applyOp({ op: "rotate", degrees: 270 })} title="向左旋转 90°" aria-label="向左旋转">
          <RotateCcw />
        </button>
        <button type="button" className="tb-btn" onClick={() => applyOp({ op: "flip", horizontal: true })} title="水平翻转" aria-label="水平翻转">
          <FlipHorizontal />
        </button>
        <button type="button" className="tb-btn" onClick={() => applyOp({ op: "flip", horizontal: false })} title="垂直翻转" aria-label="垂直翻转">
          <FlipVertical />
        </button>
        <button
          type="button"
          className={`tb-btn ${tool === "text" ? "is-active" : ""}`}
          onClick={() => setTool(tool === "text" ? "none" : "text")}
          title="添加文字（点画布放置）"
          aria-label="添加文字"
        >
          <Type />
        </button>
        <div className="toolbar-spacer" />
        <button type="button" className="tb-btn" onClick={undo} disabled={past.length === 0} title="撤销" aria-label="撤销">
          <Undo2 />
        </button>
        <button type="button" className="tb-btn" onClick={redo} disabled={future.length === 0} title="重做" aria-label="重做">
          <Redo2 />
        </button>
        <button type="button" className="tb-btn" onClick={reset} disabled={ops.length === 0} title="重置全部修改" aria-label="重置修改">
          <SlidersHorizontal />
        </button>
        <div className="toolbar-spacer" />
        <button type="button" className="tb-btn" onClick={() => void doExport(null)} disabled={busy} title="导出为新文件" aria-label="导出">
          <Save />
        </button>
        <button type="button" className="tb-btn" onClick={() => void doSaveAs()} disabled={busy} title="另存为…" aria-label="另存为">
          <FolderOpen />
        </button>
        <button type="button" className="tb-btn" onClick={onClose} title="退出编辑" aria-label="退出编辑">
          退出
        </button>
      </div>

      <div className="img-body">
        <div className="img-canvas-wrap">
          {loadError ? (
            <div className="preview-state-center">
              <div className="preview-state-error">
                <p>{loadError}</p>
                <small>{path}</small>
              </div>
            </div>
          ) : (
            <div className="img-canvas-frame">
              <canvas
                ref={canvasRef}
                className={`img-canvas ${tool === "crop" ? "is-crop" : ""}`}
                onPointerDown={onPointerDown}
                onPointerMove={onPointerMove}
                onPointerUp={onPointerUp}
              />
              {cropRect && (
                <div
                  className="img-crop-box"
                  style={{
                    left: cropRect.x,
                    top: cropRect.y,
                    width: cropRect.w,
                    height: cropRect.h,
                  }}
                />
              )}
            </div>
          )}
        </div>

        <aside className="img-params">
          <div className="sb-head">调整</div>
          <label className="img-param">
            <span>亮度</span>
            <input
              type="range"
              min={-100}
              max={100}
              value={brighten?.amount ?? 0}
              onChange={(e) => {
                const amount = Number(e.target.value);
                if (amount === 0) {
                  pushOps(ops.filter((o) => o.op !== "brighten"));
                } else {
                  applyOp({ op: "brighten", amount });
                }
              }}
            />
            <b>{brighten?.amount ?? 0}</b>
          </label>
          <label className="img-param">
            <span>对比度</span>
            <input
              type="range"
              min={50}
              max={200}
              value={Math.round((contrast?.factor ?? 1) * 100)}
              onChange={(e) => {
                const factor = Number(e.target.value) / 100;
                if (factor === 1) {
                  pushOps(ops.filter((o) => o.op !== "contrast"));
                } else {
                  applyOp({ op: "contrast", factor });
                }
              }}
            />
            <b>{Math.round((contrast?.factor ?? 1) * 100)}%</b>
          </label>
          <label className="img-param">
            <span>饱和度</span>
            <input
              type="range"
              min={0}
              max={200}
              value={Math.round((saturate?.factor ?? 1) * 100)}
              onChange={(e) => {
                const factor = Number(e.target.value) / 100;
                if (factor === 1) {
                  pushOps(ops.filter((o) => o.op !== "saturate"));
                } else {
                  applyOp({ op: "saturate", factor });
                }
              }}
            />
            <b>{Math.round((saturate?.factor ?? 1) * 100)}%</b>
          </label>
          <label className="img-param">
            <span>模糊</span>
            <input
              type="range"
              min={0}
              max={20}
              step={0.5}
              value={blur?.sigma ?? 0}
              onChange={(e) => {
                const sigma = Number(e.target.value);
                if (sigma === 0) {
                  pushOps(ops.filter((o) => o.op !== "blur"));
                } else {
                  applyOp({ op: "blur", sigma });
                }
              }}
            />
            <b>{blur?.sigma ?? 0}</b>
          </label>
          <label className="img-param">
            <span>马赛克</span>
            <input
              type="range"
              min={0}
              max={64}
              value={mosaic?.block ?? 0}
              onChange={(e) => {
                const block = Number(e.target.value);
                if (block === 0) {
                  pushOps(ops.filter((o) => o.op !== "mosaic"));
                } else {
                  applyOp({ op: "mosaic", block });
                }
              }}
            />
            <b>{mosaic?.block ?? 0}</b>
          </label>
          <button
            type="button"
            className={`img-toggle ${isGray ? "is-active" : ""}`}
            onClick={() => {
              if (isGray) {
                pushOps(ops.filter((o) => o.op !== "grayscale"));
              } else {
                applyOp({ op: "grayscale" });
              }
            }}
          >
            转黑白
          </button>

          <div className="sb-head">尺寸</div>
          <div className="img-param-row">
            <label className="img-num">
              <span>宽</span>
              <input
                type="number"
                min={1}
                value={resize?.w ?? state.w}
                onChange={(e) => {
                  const w = Math.max(1, Number(e.target.value) || 1);
                  const h = resize?.h ?? state.h;
                  applyOp({ op: "resize", w, h });
                }}
              />
            </label>
            <label className="img-num">
              <span>高</span>
              <input
                type="number"
                min={1}
                value={resize?.h ?? state.h}
                onChange={(e) => {
                  const h = Math.max(1, Number(e.target.value) || 1);
                  const w = resize?.w ?? state.w;
                  applyOp({ op: "resize", w, h });
                }}
              />
            </label>
          </div>

          <div className="sb-head">导出</div>
          <div className="img-param-row">
            <label className="img-num">
              <span>格式</span>
              <select value={format} onChange={(e) => setFormat(e.target.value)}>
                {FORMATS.map((f) => (
                  <option key={f} value={f}>
                    {f.toUpperCase()}
                  </option>
                ))}
              </select>
            </label>
            <label className="img-num">
              <span>JPEG 质量</span>
              <input
                type="number"
                min={1}
                max={100}
                disabled={format !== "jpg"}
                value={quality}
                onChange={(e) =>
                  setQuality(Math.min(100, Math.max(1, Number(e.target.value) || 90)))
                }
              />
            </label>
          </div>

          <div className="sb-head">文字</div>
          <button
            type="button"
            className={`img-toggle ${tool === "text" ? "is-active" : ""}`}
            onClick={() => setTool(tool === "text" ? "none" : "text")}
          >
            {tool === "text" ? "点击画布放置文字…" : "添加文字"}
          </button>
          {texts.length > 0 && (
            <div className="img-text-list">
              {texts.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  className={`img-text-item ${activeText === t.id ? "is-active" : ""}`}
                  onClick={() => setActiveText(t.id)}
                >
                  {t.text || "（空）"}
                </button>
              ))}
            </div>
          )}
          {activeLayer && (
            <>
              <div className="img-param-row">
                <label className="img-num">
                  <span>内容</span>
                  <input
                    value={activeLayer.text}
                    onChange={(e) => updateText(activeLayer.id, { text: e.target.value })}
                  />
                </label>
              </div>
              <div className="img-param-row">
                <label className="img-num">
                  <span>字号</span>
                  <input
                    type="number"
                    min={8}
                    value={activeLayer.size}
                    onChange={(e) =>
                      updateText(activeLayer.id, {
                        size: Math.max(8, Number(e.target.value) || 8),
                      })
                    }
                  />
                </label>
                <label className="img-num">
                  <span>颜色</span>
                  <input
                    type="color"
                    value={activeLayer.color}
                    onChange={(e) => updateText(activeLayer.id, { color: e.target.value })}
                  />
                </label>
              </div>
              <div className="img-param-row">
                <label className="img-num">
                  <span>X</span>
                  <input
                    type="number"
                    value={activeLayer.x}
                    onChange={(e) =>
                      updateText(activeLayer.id, {
                        x: Math.max(0, Number(e.target.value) || 0),
                      })
                    }
                  />
                </label>
                <label className="img-num">
                  <span>Y</span>
                  <input
                    type="number"
                    value={activeLayer.y}
                    onChange={(e) =>
                      updateText(activeLayer.id, {
                        y: Math.max(0, Number(e.target.value) || 0),
                      })
                    }
                  />
                </label>
                <button
                  type="button"
                  className="tb-btn"
                  onClick={() => {
                    setTexts((ts) => ts.filter((t) => t.id !== activeLayer.id));
                    setActiveText(null);
                  }}
                  title="删除该文字"
                  aria-label="删除该文字"
                >
                  <X className="icon-sm" />
                </button>
              </div>
            </>
          )}

          <div className="sb-head">拼接</div>
          <button type="button" className="img-toggle" onClick={() => void pickMontage()}>
            <Images className="icon-sm" /> 选择图片…
          </button>
          {montagePaths.length > 0 && (
            <>
              <div className="img-text-list">
                {montagePaths.map((p) => (
                  <div key={p} className="img-text-item is-plain">
                    <span title={p}>{p.split(/[\\/]/).pop()}</span>
                    <button
                      type="button"
                      className="tb-btn"
                      onClick={() => setMontagePaths((ps) => ps.filter((x) => x !== p))}
                      aria-label={`移除 ${p}`}
                    >
                      <X className="icon-sm" />
                    </button>
                  </div>
                ))}
              </div>
              <button
                type="button"
                className="img-toggle"
                disabled={busy}
                onClick={() => void doMontage()}
              >
                拼成一张（{montagePaths.length + 1} 张）
              </button>
            </>
          )}

          <div className="img-status">
            原图 {srcW}×{srcH} · 当前 {state.w}×{state.h} · 原文件不会被改动
          </div>
          {note && (
            <div className="img-note">
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

function defaultFormat(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() ?? "png";
  if (ext === "jpeg") return "jpg";
  if (ext === "tif") return "tiff";
  if ((FORMATS as readonly string[]).includes(ext)) return ext;
  return "png";
}

function defaultOutputName(name: string, format: string): string {
  const dot = name.lastIndexOf(".");
  const stem = dot > 0 ? name.slice(0, dot) : name;
  return `${stem}.edited.${format}`;
}

/** 另存对话框的扩展名过滤：jpeg 用 .jpg 存，盘上常见的是 .jpeg，一并放行。 */
function extList(format: string): string[] {
  return format === "jpg" ? ["jpg", "jpeg"] : [format];
}

function mimeOf(name: string): string {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "png": return "image/png";
    case "jpg":
    case "jpeg": return "image/jpeg";
    case "gif": return "image/gif";
    case "webp": return "image/webp";
    case "bmp": return "image/bmp";
    default: return "application/octet-stream";
  }
}

/**
 * Blob → base64。分块拼字符串：`String.fromCharCode(...bytes)` 一次传
 * 整个文件会爆调用栈（几 MB 的图就够炸）。
 */
async function blobToBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let bin = "";
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    bin += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(bin);
}
