// 截图编辑器：选区确认后的画布编辑（矩形/画笔/文本/涂抹/裁剪/撤销 + 色板）。
// 基底图由 Rust 按选区无损裁好（screenshot_select 返回 PNG data URL），
// 覆盖窗此时已被缩到选区大小（且不小于 Rust 侧的编辑下限 560×280）。
// 布局：窗口 = 暗底面板；上半部「舞台」放画布，底部工具条独占一条空间。
// 画布在舞台内等比适配（1:1 物理是上限），小选区也不会让工具条压到图像上。
// 标注尺寸（字/线宽/笔刷）按**屏幕显示尺寸**定义，落笔时按 disp.k 换算成
// 图像像素：大截图缩小显示时字也是 20 CSS px，不会缩成看不见的 4px。
// 提交 → PNG data URL → screenshot_commit_edited → 剪贴板。
// 基底画布(base)存"已提交"像素，可见画布 = base + 进行中预览；
// 撤销栈存 pre-op 快照（PNG dataURL，限 12 步）。
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { invoke } from "../ipc";
import { Square, MoveUpRight, Pencil, Type, Grid2x2, Crop, Undo2, X, Check } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { cx } from "../lib/cx";

export interface EditorSource {
  /** 选区无损 PNG data URL（Rust 端裁好并返回） */
  src: string;
  /** 屏缩放（物理→CSS）：笔宽/字号随屏缩放，4K 屏不至于细成发丝 */
  scale: number;
}

type Tool = "rect" | "arrow" | "pen" | "text" | "mosaic" | "crop";

// 标注笔颜色：画在截图像素上的固定色板（含黑/白两端），不随主题。
// design-guard: allow
const COLORS = ["#e5484d", "#f5a623", "#2f81f7", "#30a46c", "#111111", "#ffffff"];
const HISTORY_MAX = 12;
/** 笔画粗细档位（屏幕显示像素，实际落笔尺寸另按缩放换算）。 */
const STROKE_WIDTHS = [
  { label: "细", w: 2 },
  { label: "中", w: 4 },
  { label: "粗", w: 7 },
];
/** 文本标注的屏幕显示字号（CSS 像素）——输入框与落字保持同一视觉大小。 */
const TEXT_CSS = 20;
/** 文本输入框尺寸（CSS 像素），必须与 .se-text-input 的 width/height 一致：
 *  定位时要靠这两个数判断"这一侧还放不放得下"。 */
const TEXT_INPUT_W = 220;
const TEXT_INPUT_H = 30;

/**
 * 屏幕显示尺寸（CSS px）→ 图像物理像素。
 *
 * 图像被缩到窗口里显示时（`disp.k` 是"每物理像素对应几个 CSS 像素"），
 * 同一个视觉大小要换成更多物理像素：屏幕上 20px 的字，在缩小 4 倍显示的
 * 截图里就是 80 个物理像素。`dispK <= 0`（首帧没量到）时退回 `dpr`。
 */
export function pxForDisplay(cssPx: number, dpr: number, dispK: number): number {
  const perCss = dispK > 0 ? 1 / dispK : dpr;
  return Math.max(1, Math.round(cssPx * perCss));
}

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
  const stageRef = useRef<HTMLDivElement | null>(null);
  const historyRef = useRef<string[]>([]);
  const dragRef = useRef<{ x0: number; y0: number; lastX: number; lastY: number } | null>(null);
  const toolRef = useRef<Tool>("rect");
  const colorRef = useRef(COLORS[0]);
  const textBoxRef = useRef<{ x: number; y: number; value: string } | null>(null);

  const [dims, setDims] = useState({ w: 0, h: 0 });
  /** 舞台可用区域（CSS 像素，已扣掉内边距与工具条）：画布在此内等比适配。 */
  const [stage, setStage] = useState({ w: 0, h: 0 });
  const [tool, setTool] = useState<Tool>("rect");
  const [color, setColor] = useState(COLORS[0]);
  const [strokeW, setStrokeW] = useState(STROKE_WIDTHS[1].w);
  const [canUndo, setCanUndo] = useState(false);
  const [committing, setCommitting] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [textBox, setTextBox] = useState<{ x: number; y: number; value: string } | null>(null);

  // 舞台尺寸：画布的可用空间随窗口与工具条行数变化（工具条独占底部一条，
  // 窄窗下会换行变高），所以量它而不是猜 window.innerHeight。
  // 观察的是舞台本身：画布怎么变都不改舞台尺寸，不会形成量测回环。
  useLayoutEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const measure = () => setStage({ w: el.clientWidth, h: el.clientHeight });
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // 显示尺寸：物理 / scale = CSS 尺寸（1:1 物理是上限，小图不放大成马赛克）。
  // 窗口由 Rust 兜底到最小尺寸，小选区时窗口比图像大 → 这里负责等比缩进
  // 舞台并居中，**不让工具条压住图像**。必须精确内联宽高——canvas 上
  // object-fit/max-* 的 letterbox 会破坏 getBoundingClientRect 坐标映射。
  const disp = (() => {
    if (!dims.w || !dims.h) return { w: 0, h: 0, k: 0 };
    const natW = dims.w / source.scale;
    const natH = dims.h / source.scale;
    // 舞台还没量到（首帧）时按窗口兜底，避免画布先按 0 尺寸消失一帧。
    const availW = stage.w || window.innerWidth;
    const availH = stage.h || window.innerHeight;
    const k = Math.min(1, availW / natW, availH / natH);
    const w = Math.max(1, Math.round(natW * k));
    const h = Math.max(1, Math.round(natH * k));
    return { w, h, k: w / dims.w };
  })();

  toolRef.current = tool;
  colorRef.current = color;
  textBoxRef.current = textBox;

  // 标注尺寸一律按**屏幕显示尺寸**（CSS 像素）定义，再换算成图像物理像素。
  // 大截图会被缩到窗口里显示（disp.k 可能只有 0.2），此前按物理像素定义
  // 尺寸 → 4000px 宽的截图里 20px 文字在屏幕上只有 4.4px，打字时输入框和
  // 落字都"看不见"（2026-10-28 用户报）。换算后屏幕上恒为 20px。
  const lineW = Math.max(2, pxForDisplay(strokeW, source.scale, disp.k));
  const TEXT_PX = Math.max(12, pxForDisplay(TEXT_CSS, source.scale, disp.k));
  const BRUSH = Math.max(8, pxForDisplay(strokeW * 4, source.scale, disp.k));

  // 文本输入框定位：默认从点击点向右下展开，舞台右/下侧放不下时翻到左/上侧。
  // 输入框是固定 220×30（不随内容变宽），所以能直接算出够不够放 —— 之前用
  // input 默认宽度（约 20 字符 = 267px），点画布右半边就顶出窗口，正在打的字
  // 被窗口边缘切掉（2026-10-28 实测）。
  const textInputPos = (() => {
    if (!textBox) return null;
    const px = textBox.x * disp.k;
    const py = textBox.y * disp.k;
    // 点击点到舞台右/下边缘的距离（画布在舞台内居中，先补回两侧留白）。
    const roomRight = (stage.w - disp.w) / 2 + (disp.w - px);
    const roomBottom = (stage.h - disp.h) / 2 + (disp.h - py);
    return {
      left: roomRight < TEXT_INPUT_W ? px - TEXT_INPUT_W : px,
      top: roomBottom < TEXT_INPUT_H ? py - TEXT_INPUT_H : py,
    };
  })();

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

  // 挂载：载入选区无损 PNG → 建基底画布（尺寸 = 图片实际像素）→ 重建涂抹源
  // → 首绘。载入前显示「正在准备图像…」。
  useEffect(() => {
    let cancelled = false;
    const img = new Image();
    img.onload = () => {
      if (cancelled) return;
      const base = document.createElement("canvas");
      base.width = img.naturalWidth;
      base.height = img.naturalHeight;
      base.getContext("2d")?.drawImage(img, 0, 0);
      baseRef.current = base;
      buildMosaic(base);
      const canvas = canvasRef.current;
      if (canvas) {
        canvas.width = base.width;
        canvas.height = base.height;
      }
      setDims({ w: base.width, h: base.height });
      redraw();
    };
    img.onerror = () => {
      if (!cancelled) setErr("截图图像加载失败，请重新截图。");
    };
    img.src = source.src;
    return () => {
      cancelled = true;
    };
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
    // 画布还没量到尺寸（首帧）时 rect 是 0×0，除法会给出 NaN 坐标。
    if (!rect.width || !rect.height) return { x: 0, y: 0 };
    return {
      x: Math.round(((e.clientX - rect.left) * canvas.width) / rect.width),
      y: Math.round(((e.clientY - rect.top) * canvas.height) / rect.height),
    };
  };

  /** 箭头：线身 + 实心三角头（张角 ±~25°）。预览与提交共用同一函数。 */
  const drawArrow = (
    ctx: CanvasRenderingContext2D,
    x0: number, y0: number, x1: number, y1: number,
  ) => {
    const angle = Math.atan2(y1 - y0, x1 - x0);
    const head = Math.max(lineW * 3.5, 10);
    ctx.strokeStyle = colorRef.current;
    ctx.fillStyle = colorRef.current;
    ctx.lineWidth = lineW;
    ctx.lineCap = "round";
    ctx.beginPath();
    ctx.moveTo(x0, y0);
    ctx.lineTo(x1, y1);
    ctx.stroke();
    ctx.beginPath();
    ctx.moveTo(x1, y1);
    ctx.lineTo(x1 - head * Math.cos(angle - Math.PI / 7), y1 - head * Math.sin(angle - Math.PI / 7));
    ctx.lineTo(x1 - head * Math.cos(angle + Math.PI / 7), y1 - head * Math.sin(angle + Math.PI / 7));
    ctx.closePath();
    ctx.fill();
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
      // 必须吃掉 mousedown 的默认聚焦行为：canvas 不可聚焦，浏览器会在本次
      // mousedown 的默认动作里把焦点交给 body —— 而输入框恰好在同一次事件里
      // 挂载并 autoFocus，于是焦点一到手就被夺走，`onBlur` 拿着空串把输入框
      // 关掉。表现就是「选文本工具后点画布什么都没发生」（2026-10-28 实测：
      // mousedown 命中 CANVAS.se-canvas，但 .se-text-input 从没进 DOM）。
      e.preventDefault();
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
        ctx.lineWidth = lineW;
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
    } else if (tool === "arrow") {
      redraw((ctx) => drawArrow(ctx, drag.x0, drag.y0, p.x, p.y));
    } else if (tool === "rect" || tool === "crop") {
      redraw((ctx) => {
        const x = Math.min(drag.x0, p.x);
        const y = Math.min(drag.y0, p.y);
        const w = Math.abs(p.x - drag.x0);
        const h = Math.abs(p.y - drag.y0);
        if (tool === "rect") {
          ctx.strokeStyle = colorRef.current;
          ctx.lineWidth = lineW;
          ctx.strokeRect(x, y, w, h);
        } else {
          // 裁剪预览：选框外压暗 + 白框
          ctx.beginPath();
          ctx.rect(0, 0, base.width, base.height);
          ctx.rect(x, y, w, h);
          // 裁剪遮罩画在截图位图上，恒定压暗 + 白框，不随主题。design-guard: allow
          ctx.fillStyle = "rgba(0,0,0,0.55)";
          ctx.fill("evenodd");
          // design-guard: allow
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
          ctx.lineWidth = lineW;
          ctx.strokeRect(x, y, w, h);
        }
      }
      redraw();
    } else if (tool === "arrow") {
      const p = toImg(e);
      if (Math.hypot(p.x - drag.x0, p.y - drag.y0) > 6) {
        pushHistory(base.toDataURL("image/png"));
        const ctx = base.getContext("2d");
        if (ctx) drawArrow(ctx, drag.x0, drag.y0, p.x, p.y);
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
    if (!canvas || committing || !baseRef.current) return;
    commitTextBox();
    setCommitting(true);
    setErr(null);
    const dataUrl = canvas.toDataURL("image/png");
    invoke("screenshot_commit_edited", { dataUrl }).catch((e) => {
      setErr(`提交失败：${e instanceof Error ? e.message : String(e)}，请重试。`);
      setCommitting(false);
    });
  };

  const tools: { id: Tool; label: string; title: string; icon: LucideIcon }[] = [
    { id: "rect", label: "框选", title: "画矩形框", icon: Square },
    { id: "arrow", label: "箭头", title: "拖拽画指示箭头", icon: MoveUpRight },
    { id: "pen", label: "画笔", title: "自由画笔", icon: Pencil },
    { id: "text", label: "文本", title: "点击画布输入文字", icon: Type },
    { id: "mosaic", label: "涂抹", title: "马赛克涂抹遮蔽隐私", icon: Grid2x2 },
    { id: "crop", label: "裁剪", title: "拖拽框选后松开完成裁剪", icon: Crop },
  ];

  return (
    <div className="se-root" onMouseMove={onMouseMove} onMouseUp={onMouseUp}>
      {dims.w === 0 && !err && <div className="so-loading" aria-busy="true">正在准备图像…</div>}
      {/* 舞台：工具条之外的全部空间，画布在其中等比居中（量它算显示尺寸）。 */}
      <div className="se-stage" ref={stageRef}>
        <div className="se-canvas-wrap" style={{ width: disp.w || undefined }}>
          <canvas
            ref={canvasRef}
            className={cx("se-canvas", tool === "text" && "tool-text")}
            style={{ width: disp.w || undefined, height: disp.h || undefined }}
            onMouseDown={onMouseDown}
          />
          {textBox && textInputPos && (
            <input
              className="se-text-input"
              autoFocus
              value={textBox.value}
              style={{
                left: textInputPos.left,
                top: textInputPos.top,
                // 输入框字号固定为屏幕显示字号：跟落字后的视觉大小一致。
                fontSize: TEXT_CSS,
                color,
              }}
              onChange={(e) => setTextBox({ ...textBox, value: e.target.value })}
              onKeyDown={(e) => {
                if (e.key === "Enter") commitTextBox();
              }}
              onBlur={commitTextBox}
              onMouseDown={(e) => e.stopPropagation()}
              placeholder="输入文字，Enter 确认"
            />
          )}
        </div>
      </div>

      {err && <div className="se-err">{err}</div>}

      <div className="se-toolbar" onMouseDown={(e) => e.stopPropagation()}>
        {/* 分组靠底色差分隔（不画分隔线）：工具 / 色板 / 笔宽 / 动作。 */}
        <div className="se-group">
          {tools.map((t) => {
            const Icon = t.icon;
            return (
              <button
                key={t.id}
                className={cx("se-tool", tool === t.id && "active")}
                title={t.title}
                aria-label={t.title}
                aria-pressed={tool === t.id}
                onClick={() => {
                  commitTextBox();
                  setTool(t.id);
                }}
              >
                <Icon size={16} aria-hidden="true" />
                <span className="se-label">{t.label}</span>
              </button>
            );
          })}
        </div>

        <div className="se-group">
          {COLORS.map((c) => (
            <button
              key={c}
              className={cx("se-color", color === c && "active")}
              style={{ background: c }}
              title={`颜色 ${c}`}
              aria-label={`标注颜色 ${c}`}
              aria-pressed={color === c}
              onClick={() => setColor(c)}
            />
          ))}
        </div>

        <div className="se-group" title="笔画粗细（框选/箭头/画笔/涂抹共用）">
          {STROKE_WIDTHS.map((s) => (
            <button
              key={s.w}
              className={cx("se-width", strokeW === s.w && "active")}
              title={`粗细：${s.label}`}
              aria-label={`笔画粗细 ${s.label}`}
              aria-pressed={strokeW === s.w}
              onClick={() => setStrokeW(s.w)}
            >
              <span style={{ width: 4 + s.w * 2, height: 4 + s.w * 2 }} />
            </button>
          ))}
        </div>

        <div className="se-group">
          <button
            className="se-icon-btn"
            title="撤销上一步"
            aria-label="撤销上一步"
            disabled={!canUndo}
            onClick={undo}
          >
            <Undo2 size={16} aria-hidden="true" />
          </button>
          <button
            className="se-icon-btn"
            title="放弃本次截图（ESC）"
            aria-label="放弃本次截图"
            onClick={onCancel}
          >
            <X size={16} aria-hidden="true" />
          </button>
          <button
            className="se-commit"
            title="编辑完成，复制到剪贴板"
            onClick={commit}
            disabled={committing || dims.w === 0}
          >
            <Check size={16} aria-hidden="true" />
            <span className="se-label">{committing ? "复制中…" : "复制"}</span>
          </button>
        </div>
      </div>
    </div>
  );
}
