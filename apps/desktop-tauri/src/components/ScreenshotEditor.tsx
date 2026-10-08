// 截图编辑器：选区确认后的画布编辑（矩形/画笔/文本/涂抹/裁剪/撤销 + 色板）。
// 画布 = 选区物理像素；显示 = 逻辑尺寸（/scale）。提交 → PNG data URL →
// screenshot_commit_edited → 剪贴板。基底画布(base)存"已提交"像素，
// 可见画布 = base + 进行中预览；撤销栈存 pre-op 快照（PNG dataURL，限 12 步）。
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { cx } from "../lib/cx";

export interface EditorSource {
  /** 全帧照片 data URL */
  dataUrl: string;
  /** 选区在帧图内的物理像素矩形 */
  sx: number;
  sy: number;
  sw: number;
  sh: number;
  /** 显示缩放（物理→逻辑） */
  scale: number;
}

type Tool = "rect" | "pen" | "text" | "mosaic" | "crop";

const COLORS = ["#e5484d", "#f5a623", "#2f81f7", "#30a46c", "#111111", "#ffffff"];
const HISTORY_MAX = 12;

export function ScreenshotEditor({
  source,
  onCancel,
}: {
  source: EditorSource;
  onCancel: () => void;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const baseRef = useRef<HTMLCanvasElement | null>(null);
  const mosaicRef = useRef<HTMLCanvasElement | null>(null);
  const historyRef = useRef<string[]>([]);
  const dragRef = useRef<{ x0: number; y0: number; lastX: number; lastY: number } | null>(null);
  const toolRef = useRef<Tool>("rect");
  const colorRef = useRef(COLORS[0]);
  const textBoxRef = useRef<{ x: number; y: number; value: string } | null>(null);

  const [dims, setDims] = useState({ w: 0, h: 0 });
  const [tool, setTool] = useState<Tool>("rect");
  const [color, setColor] = useState(COLORS[0]);
  const [canUndo, setCanUndo] = useState(false);
  const [committing, setCommitting] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [textBox, setTextBox] = useState<{ x: number; y: number; value: string } | null>(null);

  // 显示尺寸：物理/scale 后若超窗（多屏 DPI 混排等边界），等比缩进窗内。
  // 必须精确内联宽高——canvas 上 object-fit/max-* 的 letterbox 会破坏
  // getBoundingClientRect 坐标映射。
  const disp = (() => {
    if (!dims.w || !dims.h) return { w: 0, h: 0, k: 0 };
    const natW = dims.w / source.scale;
    const natH = dims.h / source.scale;
    const k = Math.min(1, (window.innerWidth - 48) / natW, (window.innerHeight - 140) / natH);
    return { w: Math.round(natW * k), h: Math.round(natH * k), k: (natW * k) / dims.w };
  })();

  toolRef.current = tool;
  colorRef.current = color;
  textBoxRef.current = textBox;

  // 线宽/字号/笔刷随屏缩放：4K 屏逻辑显示只有一半，固定物理值会细成发丝。
  const LINE = Math.max(3, Math.round(3 * source.scale));
  const TEXT_PX = Math.max(18, Math.round(20 * source.scale));
  const BRUSH = Math.max(10, Math.round(14 * source.scale));

  /** 由基底重建全尺寸像素化画布（涂抹取样源）。 */
  const buildMosaic = useCallback((src: HTMLCanvasElement) => {
    const small = document.createElement("canvas");
    small.width = Math.max(1, Math.round(src.width / 12));
    small.height = Math.max(1, Math.round(src.height / 12));
    small.getContext("2d")?.drawImage(src, 0, 0, small.width, small.height);
    const full = document.createElement("canvas");
    full.width = src.width;
    full.height = src.height;
    const fctx = full.getContext("2d");
    if (fctx) {
      fctx.imageSmoothingEnabled = false;
      fctx.drawImage(small, 0, 0, full.width, full.height);
    }
    mosaicRef.current = full;
  }, []);

  /** 可见画布 = base（+ 可选预览绘制）。 */
  const redraw = useCallback((preview?: (ctx: CanvasRenderingContext2D) => void) => {
    const canvas = canvasRef.current;
    const base = baseRef.current;
    if (!canvas || !base) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, canvas.width, canvas.height);
    ctx.drawImage(base, 0, 0);
    if (preview) {
      ctx.save();
      preview(ctx);
      ctx.restore();
    }
  }, []);

  // 挂载：加载全帧图 → 裁出选区为基底 → 重建涂抹源 → 首绘。
  useEffect(() => {
    const img = new Image();
    img.onload = () => {
      const base = document.createElement("canvas");
      base.width = source.sw;
      base.height = source.sh;
      base.getContext("2d")?.drawImage(
        img,
        source.sx, source.sy, source.sw, source.sh,
        0, 0, source.sw, source.sh,
      );
      baseRef.current = base;
      buildMosaic(base);
      const canvas = canvasRef.current;
      if (canvas) {
        canvas.width = source.sw;
        canvas.height = source.sh;
      }
      setDims({ w: source.sw, h: source.sh });
      redraw();
    };
    img.onerror = () => setErr("截图图像加载失败");
    img.src = source.dataUrl;
  }, [source, buildMosaic, redraw]);

  const pushHistory = useCallback((preOp: string) => {
    historyRef.current.push(preOp);
    if (historyRef.current.length > HISTORY_MAX) historyRef.current.shift();
    setCanUndo(true);
  }, []);

  const adoptBase = useCallback(
    (base: HTMLCanvasElement) => {
      baseRef.current = base;
      buildMosaic(base);
      const canvas = canvasRef.current;
      if (canvas) {
        canvas.width = base.width;
        canvas.height = base.height;
      }
      setDims({ w: base.width, h: base.height });
      redraw();
    },
    [buildMosaic, redraw],
  );

  const undo = useCallback(() => {
    const snap = historyRef.current.pop();
    setCanUndo(historyRef.current.length > 0);
    if (!snap) return;
    const img = new Image();
    img.onload = () => {
      const base = document.createElement("canvas");
      base.width = img.naturalWidth;
      base.height = img.naturalHeight;
      base.getContext("2d")?.drawImage(img, 0, 0);
      adoptBase(base);
    };
    img.src = snap;
  }, [adoptBase]);

  /** 事件坐标 → 画布物理像素。 */
  const toImg = (e: React.MouseEvent) => {
    const canvas = canvasRef.current;
    if (!canvas) return { x: 0, y: 0 };
    const rect = canvas.getBoundingClientRect();
    return {
      x: Math.round(((e.clientX - rect.left) * canvas.width) / rect.width),
      y: Math.round(((e.clientY - rect.top) * canvas.height) / rect.height),
    };
  };

  const commitTextBox = useCallback(() => {
    const tb = textBoxRef.current;
    const base = baseRef.current;
    if (!tb || !base) return;
    const value = tb.value.trim();
    setTextBox(null);
    if (!value) return;
    pushHistory(base.toDataURL("image/png"));
    const ctx = base.getContext("2d");
    if (!ctx) return;
    ctx.fillStyle = colorRef.current;
    ctx.textBaseline = "top";
    ctx.font = `bold ${TEXT_PX}px "Microsoft YaHei", "Segoe UI", sans-serif`;
    ctx.fillText(value, tb.x, tb.y);
    redraw();
  }, [pushHistory, redraw, TEXT_PX]);

  const onMouseDown = (e: React.MouseEvent) => {
    if (committing || err) return;
    const p = toImg(e);
    if (tool === "text") {
      commitTextBox();
      setTextBox({ x: p.x, y: p.y, value: "" });
      return;
    }
    dragRef.current = { x0: p.x, y0: p.y, lastX: p.x, lastY: p.y };
    if (tool === "pen" || tool === "mosaic") {
      // 破坏性工具：按下即入栈（pre-op 快照），拖动过程直接写基底。
      const base = baseRef.current;
      if (base) pushHistory(base.toDataURL("image/png"));
      if (tool === "mosaic") {
        const ctx = base?.getContext("2d");
        const mosaic = mosaicRef.current;
        if (ctx && mosaic) {
          ctx.save();
          ctx.beginPath();
          ctx.arc(p.x, p.y, BRUSH, 0, Math.PI * 2);
          ctx.clip();
          ctx.drawImage(mosaic, 0, 0);
          ctx.restore();
          redraw();
        }
      }
    }
  };

  const onMouseMove = (e: React.MouseEvent) => {
    const drag = dragRef.current;
    if (!drag || committing || err) return;
    const p = toImg(e);
    const base = baseRef.current;
    if (!base) return;
    if (tool === "pen") {
      const ctx = base.getContext("2d");
      if (ctx) {
        ctx.strokeStyle = colorRef.current;
        ctx.lineWidth = LINE;
        ctx.lineCap = "round";
        ctx.lineJoin = "round";
        ctx.beginPath();
        ctx.moveTo(drag.lastX, drag.lastY);
        ctx.lineTo(p.x, p.y);
        ctx.stroke();
        redraw();
      }
    } else if (tool === "mosaic") {
      const ctx = base.getContext("2d");
      const mosaic = mosaicRef.current;
      if (ctx && mosaic) {
        ctx.save();
        ctx.beginPath();
        ctx.arc(p.x, p.y, BRUSH, 0, Math.PI * 2);
        ctx.clip();
        ctx.drawImage(mosaic, 0, 0);
        ctx.restore();
        redraw();
      }
    } else if (tool === "rect" || tool === "crop") {
      redraw((ctx) => {
        const x = Math.min(drag.x0, p.x);
        const y = Math.min(drag.y0, p.y);
        const w = Math.abs(p.x - drag.x0);
        const h = Math.abs(p.y - drag.y0);
        if (tool === "rect") {
          ctx.strokeStyle = colorRef.current;
          ctx.lineWidth = LINE;
          ctx.strokeRect(x, y, w, h);
        } else {
          // 裁剪预览：选框外压暗 + 白框
          ctx.beginPath();
          ctx.rect(0, 0, base.width, base.height);
          ctx.rect(x, y, w, h);
          ctx.fillStyle = "rgba(0,0,0,0.55)";
          ctx.fill("evenodd");
          ctx.strokeStyle = "#ffffff";
          ctx.lineWidth = 1;
          ctx.strokeRect(x, y, w, h);
        }
      });
    }
    drag.lastX = p.x;
    drag.lastY = p.y;
  };

  const onMouseUp = (e: React.MouseEvent) => {
    const drag = dragRef.current;
    dragRef.current = null;
    if (!drag || committing || err) return;
    const base = baseRef.current;
    if (!base) return;
    if (tool === "rect") {
      const p = toImg(e);
      const x = Math.min(drag.x0, p.x);
      const y = Math.min(drag.y0, p.y);
      const w = Math.abs(p.x - drag.x0);
      const h = Math.abs(p.y - drag.y0);
      if (w > 3 && h > 3) {
        pushHistory(base.toDataURL("image/png"));
        const ctx = base.getContext("2d");
        if (ctx) {
          ctx.strokeStyle = colorRef.current;
          ctx.lineWidth = LINE;
          ctx.strokeRect(x, y, w, h);
        }
      }
      redraw();
    } else if (tool === "crop") {
      const p = toImg(e);
      const x = Math.max(0, Math.min(drag.x0, p.x));
      const y = Math.max(0, Math.min(drag.y0, p.y));
      const w = Math.min(base.width - x, Math.abs(p.x - drag.x0));
      const h = Math.min(base.height - y, Math.abs(p.y - drag.y0));
      if (w > 10 && h > 10 && (w < base.width || h < base.height)) {
        pushHistory(base.toDataURL("image/png"));
        const next = document.createElement("canvas");
        next.width = Math.round(w);
        next.height = Math.round(h);
        next.getContext("2d")?.drawImage(base, x, y, next.width, next.height, 0, 0, next.width, next.height);
        adoptBase(next);
      } else {
        redraw();
      }
    }
  };

  const commit = () => {
    const canvas = canvasRef.current;
    if (!canvas || committing) return;
    commitTextBox();
    setCommitting(true);
    setErr(null);
    const dataUrl = canvas.toDataURL("image/png");
    invoke("screenshot_commit_edited", { dataUrl }).catch((e) => {
      setErr(String(e));
      setCommitting(false);
    });
  };

  const tools: { id: Tool; label: string; title: string }[] = [
    { id: "rect", label: "▭ 框选", title: "画矩形框" },
    { id: "pen", label: "✎ 画笔", title: "自由画笔" },
    { id: "text", label: "T 文本", title: "点击画布输入文字" },
    { id: "mosaic", label: "▓ 涂抹", title: "马赛克涂抹遮蔽隐私" },
    { id: "crop", label: "✂ 裁剪", title: "拖拽框选后松开完成裁剪" },
  ];

  return (
    <div className="se-root" onMouseMove={onMouseMove} onMouseUp={onMouseUp}>
      <div className="se-canvas-wrap" style={{ width: disp.w || undefined }}>
        <canvas
          ref={canvasRef}
          className={cx("se-canvas", tool === "text" && "tool-text")}
          style={{ width: disp.w || undefined, height: disp.h || undefined }}
          onMouseDown={onMouseDown}
        />
        {textBox && (
          <input
            className="se-text-input"
            autoFocus
            value={textBox.value}
            style={{
              left: textBox.x * disp.k,
              top: textBox.y * disp.k,
              fontSize: TEXT_PX * disp.k,
              color,
            }}
            onChange={(e) => setTextBox({ ...textBox, value: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter") commitTextBox();
            }}
            onBlur={commitTextBox}
            onMouseDown={(e) => e.stopPropagation()}
            placeholder="输入文字，Enter 确定"
          />
        )}
      </div>

      {err && <div className="se-err">提交失败：{err}</div>}

      <div className="se-toolbar" onMouseDown={(e) => e.stopPropagation()}>
        {tools.map((t) => (
          <button
            key={t.id}
            className={cx("se-tool", tool === t.id && "active")}
            title={t.title}
            onClick={() => {
              commitTextBox();
              setTool(t.id);
            }}
          >
            {t.label}
          </button>
        ))}
        <div className="se-divider" />
        {COLORS.map((c) => (
          <button
            key={c}
            className={cx("se-color", color === c && "active")}
            style={{ background: c }}
            title={`颜色 ${c}`}
            onClick={() => setColor(c)}
          />
        ))}
        <div className="se-divider" />
        <button
          className="se-tool"
          title="撤销上一步"
          disabled={!canUndo}
          onClick={undo}
        >
          ↶ 撤销
        </button>
        <div className="se-divider" />
        <button className="se-tool" title="放弃本次截图（ESC）" onClick={onCancel}>
          ✕
        </button>
        <button className="se-commit" title="编辑完成，复制到剪贴板" onClick={commit} disabled={committing}>
          {committing ? "复制中…" : "✓ 复制"}
        </button>
      </div>
    </div>
  );
}
