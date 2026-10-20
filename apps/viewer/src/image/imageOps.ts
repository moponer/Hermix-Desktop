// 图片 op 模型（前端侧）——与 Rust 侧 `apps/viewer/src-tauri/src/image_ops.rs`
// 的 `ImageOp` **一一对应**（serde 的 `tag = "op"` + camelCase 变体名）。
//
// 两份实现靠**公式约定**钉死，公式写在这里与 Rust 文件的同名注释里：
//   - 亮度 = 加法（正数变亮）
//   - 对比度 = 以 127.5 为中心的系数（1.0 = 不变）
//   - 灰度 = Rec.601 亮度（0.299R + 0.587G + 0.114B）
//   - 饱和度 = 围绕灰度轴插值：gray + (c - gray) * factor，
//     其中 gray = (max + min) / 2（HSL 的 L）
//
// 预览在前端 Canvas（要 60fps），导出在 Rust（权威）。预览用降采样画布，
// 所以预览与导出可能有插值级差异——**导出结果才是准的**。

export type ImageOp =
  | { op: "crop"; x: number; y: number; w: number; h: number }
  | { op: "rotate"; degrees: number }
  | { op: "flip"; horizontal: boolean }
  | { op: "resize"; w: number; h: number }
  | { op: "grayscale" }
  | { op: "brighten"; amount: number }
  | { op: "contrast"; factor: number }
  | { op: "saturate"; factor: number }
  | { op: "blur"; sigma: number }
  | { op: "mosaic"; block: number }
  | { op: "binarize"; threshold: number };

/**
 * 可编辑的栅格格式。
 *
 * SVG 不在其中：它是矢量，`image` 解不了，Rust 侧导出会直接失败——
 * 与其给一个点开就报错的按钮，不如不给。
 */
export const EDITABLE_FORMATS = ["png", "jpg", "webp", "bmp", "tiff", "gif", "ico"] as const;

/** 这个文件能不能进图片编辑器（按扩展名判定）。 */
export function isEditableImage(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (ext === "jpeg") return true;
  if (ext === "tif") return true;
  return (EDITABLE_FORMATS as readonly string[]).includes(ext);
}

// ════════════════════════════════════════════════════════════════════
//  文字图层
// ════════════════════════════════════════════════════════════════════

/**
 * 文字图层：**独立于 op 链**。
 *
 * op 链是前后端一一对应的（Rust 逐条执行），而文字只有浏览器画得对
 * ——系统字体、中文排版、字重，Rust 侧要复刻就得引字体解析依赖。
 * 所以文字不进 op 链，而是作为独立图层：前端画，导出时整张合成图交给
 * Rust 转格式落盘。
 */
export interface TextLayer {
  id: number;
  text: string;
  /** 原图空间坐标（左上角锚点） */
  x: number;
  y: number;
  /** 字号（原图空间像素） */
  size: number;
  color: string;
}

/** 兜底色（拿不到主题令牌时）。放在 .ts 里：TSX 字面色值是门禁红线。 */
const FALLBACK_TEXT_COLOR = "#111827";

/**
 * 从主题令牌取颜色：文字默认色跟随明暗主题，而不是写死一个色值
 * （写死的话深色背景下新加的字会看不见）。
 */
export function readTokenColor(token: string): string {
  if (typeof window === "undefined") return FALLBACK_TEXT_COLOR;
  const v = getComputedStyle(document.documentElement).getPropertyValue(token).trim();
  return v || FALLBACK_TEXT_COLOR;
}

/** 在画布上绘制文字图层。坐标与字号都按原图空间给，调用方乘 scale。 */
export function drawTextLayers(
  ctx: CanvasRenderingContext2D,
  layers: TextLayer[],
  scale: number,
): void {
  for (const layer of layers) {
    if (!layer.text) continue;
    ctx.save();
    ctx.font = `${Math.max(1, Math.round(layer.size * scale))}px "Segoe UI", system-ui, sans-serif`;
    ctx.fillStyle = layer.color;
    ctx.textBaseline = "top";
    ctx.fillText(layer.text, layer.x * scale, layer.y * scale);
    ctx.restore();
  }
}

// ════════════════════════════════════════════════════════════════════
//  马赛克笔（局部打码）
// ════════════════════════════════════════════════════════════════════

/**
 * 一笔马赛克涂抹。
 *
 * 坐标是**状态空间**（op 链折叠后的画布尺寸，单位＝原图像素）——
 * 与文字图层同一套约定：预览按 scale 放大绘制，导出按 1:1 绘制，
 * 两边看到的是同一份结果。
 */
export interface MosaicStroke {
  /** 马赛克格子边长（状态空间像素） */
  block: number;
  /** 笔刷半径（状态空间像素） */
  radius: number;
  /** 轨迹点（状态空间坐标） */
  points: { x: number; y: number }[];
  /** 落笔时的状态画布尺寸：之后若发生裁剪/缩放，按比例映射回去 */
  refW: number;
  refH: number;
}

/**
 * 把马赛克笔的涂抹叠到画布上（就地修改 canvas）。
 *
 * 做法：**整图马赛克算一次当底图**，再用笔刷轨迹做蒙版，只把蒙版覆盖到
 * 的像素换成底图像素——涂过的地方马赛克，没涂的地方原样保留。
 *
 * 为什么用 canvas 合成（destination-in）而不是逐像素 JS：
 * 整图马赛克只算一次、混合走 GPU，拖动时能保持流畅；
 * 逐像素会让 1200×800 的画布每帧跑百万次循环。
 *
 * @param scale 状态空间 → 画布设备像素的换算系数（预览 <1，导出 =1）
 * @param stateW/stateH 当前状态画布尺寸（笔画按 ref 尺寸等比映射过来）
 */
export function applyMosaicStrokes(
  canvas: HTMLCanvasElement,
  strokes: MosaicStroke[],
  scale: number,
  stateW: number,
  stateH: number,
): void {
  if (strokes.length === 0) return;
  const ctx = canvas.getContext("2d");
  if (!ctx) return;
  // 按格子大小分组：同尺寸的笔画共用一份马赛克底图
  const groups = new Map<number, MosaicStroke[]>();
  for (const s of strokes) {
    const key = Math.max(1, Math.round(s.block));
    const list = groups.get(key);
    if (list) list.push(s);
    else groups.set(key, [s]);
  }
  for (const [block, list] of groups) {
    const base = applyMosaic(canvas, Math.max(1, Math.round(block * scale)));
    for (const s of list) {
      blendStroke(ctx, canvas, base, s, scale, stateW, stateH);
    }
  }
}

/**
 * 把一笔涂抹混进画布：取笔刷包围盒那块底图，按笔刷蒙版裁掉未涂区域，再叠回。
 *
 * 边缘是**软过渡**（蒙版带抗锯齿 alpha，source-over 混合）——
 * 硬边会让人一眼看出"贴了块马赛克上去"，软边才像真的涂抹。
 */
function blendStroke(
  ctx: CanvasRenderingContext2D,
  dst: HTMLCanvasElement,
  base: HTMLCanvasElement,
  stroke: MosaicStroke,
  scale: number,
  stateW: number,
  stateH: number,
): void {
  const pts = stroke.points;
  if (pts.length === 0) return;
  // 落笔之后又改了画布尺寸（裁剪/缩放）→ 笔画等比映射过去
  const kx = stateW / Math.max(1, stroke.refW);
  const ky = stateH / Math.max(1, stroke.refH);
  const r = Math.max(0.5, stroke.radius * Math.min(kx, ky)) * scale;

  const px: number[] = [];
  const py: number[] = [];
  let minX = Infinity;
  let minY = Infinity;
  let maxX = -Infinity;
  let maxY = -Infinity;
  for (const p of pts) {
    const x = p.x * kx * scale;
    const y = p.y * ky * scale;
    px.push(x);
    py.push(y);
    if (x < minX) minX = x;
    if (x > maxX) maxX = x;
    if (y < minY) minY = y;
    if (y > maxY) maxY = y;
  }
  const x0 = Math.max(0, Math.floor(minX - r));
  const y0 = Math.max(0, Math.floor(minY - r));
  const x1 = Math.min(dst.width, Math.ceil(maxX + r));
  const y1 = Math.min(dst.height, Math.ceil(maxY + r));
  const w = x1 - x0;
  const h = y1 - y0;
  if (w <= 0 || h <= 0) return;

  // 笔刷蒙版：圆头折线（lineCap/lineJoin=round）＝一串圆
  const mask = newCanvas(w, h);
  const mctx = mask.getContext("2d")!;
  mctx.strokeStyle = "#fff";
  mctx.lineWidth = r * 2;
  mctx.lineCap = "round";
  mctx.lineJoin = "round";
  mctx.beginPath();
  mctx.moveTo(px[0] - x0, py[0] - y0);
  for (let i = 1; i < px.length; i++) mctx.lineTo(px[i] - x0, py[i] - y0);
  if (px.length === 1) mctx.lineTo(px[0] - x0 + 0.01, py[0] - y0);
  mctx.stroke();

  // 底图只取这一块，再按蒙版裁掉没涂到的像素
  const region = newCanvas(w, h);
  const rctx = region.getContext("2d")!;
  rctx.drawImage(base, x0, y0, w, h, 0, 0, w, h);
  rctx.globalCompositeOperation = "destination-in";
  rctx.drawImage(mask, 0, 0);
  rctx.globalCompositeOperation = "source-over";

  ctx.drawImage(region, x0, y0);
}

/** 像素级 op（改 RGB，不改画布尺寸）。 */
const PIXEL_OPS = new Set(["grayscale", "brighten", "contrast", "saturate"]);

export function isPixelOp(op: ImageOp): boolean {
  return PIXEL_OPS.has(op.op);
}

/** 几何 op（改画布尺寸）。 */
export function isGeometryOp(op: ImageOp): boolean {
  return op.op === "crop" || op.op === "rotate" || op.op === "flip" || op.op === "resize";
}

/**
 * 折叠 op 链后的**原图像素空间**尺寸。
 *
 * 为什么要在原图空间算：op 里的坐标（裁剪框、目标宽高）都是原图空间的
 * 数值，前端只在显示时按比例缩放；Rust 导出时拿到的也是原图空间的数值，
 * 两边一致，预览与导出才对得上。
 */
export function stateSize(
  srcW: number,
  srcH: number,
  ops: ImageOp[],
): { w: number; h: number } {
  let w = srcW;
  let h = srcH;
  for (const op of ops) {
    switch (op.op) {
      case "crop":
        w = Math.max(1, Math.round(op.w));
        h = Math.max(1, Math.round(op.h));
        break;
      case "rotate":
        if (op.degrees % 180 === 90) {
          [w, h] = [h, w];
        }
        break;
      case "resize":
        w = Math.max(1, Math.round(op.w));
        h = Math.max(1, Math.round(op.h));
        break;
      default:
        break;
    }
  }
  return { w, h };
}

/** 预览画布的降采样比例：长边不超过 maxSide。 */
export function previewScale(stateW: number, stateH: number, maxSide: number): number {
  const longest = Math.max(stateW, stateH);
  if (longest <= maxSide) return 1;
  return maxSide / longest;
}

// ════════════════════════════════════════════════════════════════════
//  Canvas 渲染
// ════════════════════════════════════════════════════════════════════

function newCanvas(w: number, h: number): HTMLCanvasElement {
  const c = document.createElement("canvas");
  c.width = Math.max(1, Math.round(w));
  c.height = Math.max(1, Math.round(h));
  return c;
}

/** 裁剪：截取矩形（左上原点，原图空间）。 */
function applyCrop(src: HTMLCanvasElement, op: Extract<ImageOp, { op: "crop" }>): HTMLCanvasElement {
  const out = newCanvas(op.w, op.h);
  out.getContext("2d")!.drawImage(src, op.x, op.y, op.w, op.h, 0, 0, op.w, op.h);
  return out;
}

/** 旋转：90° 整数倍。 */
function applyRotate(
  src: HTMLCanvasElement,
  degrees: number,
): HTMLCanvasElement {
  const swap = degrees % 180 === 90;
  const out = newCanvas(swap ? src.height : src.width, swap ? src.width : src.height);
  const ctx = out.getContext("2d")!;
  ctx.translate(out.width / 2, out.height / 2);
  ctx.rotate((degrees * Math.PI) / 180);
  ctx.drawImage(src, -src.width / 2, -src.height / 2);
  return out;
}

/** 翻转。 */
function applyFlip(src: HTMLCanvasElement, horizontal: boolean): HTMLCanvasElement {
  const out = newCanvas(src.width, src.height);
  const ctx = out.getContext("2d")!;
  ctx.translate(horizontal ? src.width : 0, horizontal ? 0 : src.height);
  ctx.scale(horizontal ? -1 : 1, horizontal ? 1 : -1);
  ctx.drawImage(src, 0, 0);
  return out;
}

/** 缩放（到指定宽高，不保比——用户要的就是这个）。 */
function applyResize(src: HTMLCanvasElement, w: number, h: number): HTMLCanvasElement {
  const out = newCanvas(w, h);
  const ctx = out.getContext("2d")!;
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(src, 0, 0, out.width, out.height);
  return out;
}

/** 模糊：用 canvas 原生 filter（与 Rust 高斯近似一致，导出以 Rust 为准）。 */
function applyBlur(src: HTMLCanvasElement, sigma: number): HTMLCanvasElement {
  const out = newCanvas(src.width, src.height);
  const ctx = out.getContext("2d")!;
  ctx.filter = `blur(${sigma}px)`;
  ctx.drawImage(src, 0, 0);
  ctx.filter = "none";
  return out;
}

/**
 * 马赛克：缩到 1/block 再按最近邻放大（等效于每块取平均色）。
 * 与 Rust 侧 `mosaic()` 同一算法。
 */
function applyMosaic(src: HTMLCanvasElement, block: number): HTMLCanvasElement {
  const b = Math.max(1, Math.round(block));
  const smallW = Math.max(1, Math.ceil(src.width / b));
  const smallH = Math.max(1, Math.ceil(src.height / b));
  const small = newCanvas(smallW, smallH);
  const sctx = small.getContext("2d")!;
  sctx.imageSmoothingQuality = "high";
  sctx.drawImage(src, 0, 0, smallW, smallH);
  const out = newCanvas(src.width, src.height);
  const ctx = out.getContext("2d")!;
  ctx.imageSmoothingEnabled = false;
  ctx.drawImage(small, 0, 0, out.width, out.height);
  ctx.imageSmoothingEnabled = true;
  return out;
}

/** 像素 op：就地改画布像素（与 Rust 侧同序执行，顺序不同结果会不同）。 */
function applyPixelOp(canvas: HTMLCanvasElement, op: ImageOp): void {
  const ctx = canvas.getContext("2d")!;
  const img = ctx.getImageData(0, 0, canvas.width, canvas.height);
  const d = img.data;
  switch (op.op) {
    case "grayscale":
      for (let i = 0; i < d.length; i += 4) {
        const g = 0.299 * d[i] + 0.587 * d[i + 1] + 0.114 * d[i + 2];
        d[i] = g;
        d[i + 1] = g;
        d[i + 2] = g;
      }
      break;
    case "brighten":
      for (let i = 0; i < d.length; i += 4) {
        d[i] = clamp8(d[i] + op.amount);
        d[i + 1] = clamp8(d[i + 1] + op.amount);
        d[i + 2] = clamp8(d[i + 2] + op.amount);
      }
      break;
    case "contrast":
      for (let i = 0; i < d.length; i += 4) {
        d[i] = clamp8((d[i] - 127.5) * op.factor + 127.5);
        d[i + 1] = clamp8((d[i + 1] - 127.5) * op.factor + 127.5);
        d[i + 2] = clamp8((d[i + 2] - 127.5) * op.factor + 127.5);
      }
      break;
    case "saturate":
      for (let i = 0; i < d.length; i += 4) {
        const r = d[i];
        const g = d[i + 1];
        const b = d[i + 2];
        const gray = (Math.max(r, g, b) + Math.min(r, g, b)) / 2;
        d[i] = clamp8(gray + (r - gray) * op.factor);
        d[i + 1] = clamp8(gray + (g - gray) * op.factor);
        d[i + 2] = clamp8(gray + (b - gray) * op.factor);
      }
      break;
    case "binarize":
      // 阈值用与 Rust 侧一致的整数亮度： (77R + 150G + 29B) >> 8
      for (let i = 0; i < d.length; i += 4) {
        const lum = (77 * d[i] + 150 * d[i + 1] + 29 * d[i + 2]) >> 8;
        const v = lum >= op.threshold ? 255 : 0;
        d[i] = v;
        d[i + 1] = v;
        d[i + 2] = v;
      }
      break;
    default:
      return;
  }
  ctx.putImageData(img, 0, 0);
}

function clamp8(v: number): number {
  return v < 0 ? 0 : v > 255 ? 255 : v;
}

/**
 * 把源图 + op 链渲染成画布。
 *
 * 缩放分两段：先把源图按 maxSide 降采样（省内存、够预览），再依次应用 op。
 * **裁剪坐标是原图空间的**，所以降采样时要按比例换算——调用方负责把
 * op 里的原图坐标换算到降采样后的画布坐标（见 ImageEditor 的 scale）。
 */
export function renderOps(
  source: CanvasImageSource,
  srcW: number,
  srcH: number,
  ops: ImageOp[],
  maxSide: number,
): HTMLCanvasElement {
  const scale = previewScale(srcW, srcH, maxSide);
  let canvas = newCanvas(srcW * scale, srcH * scale);
  let ctx = canvas.getContext("2d")!;
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(source, 0, 0, canvas.width, canvas.height);
  for (const op of ops) {
    switch (op.op) {
      case "crop": {
        const c = applyCrop(canvas, {
          op: "crop",
          x: op.x * scale,
          y: op.y * scale,
          w: op.w * scale,
          h: op.h * scale,
        });
        canvas = c;
        break;
      }
      case "rotate":
        canvas = applyRotate(canvas, op.degrees);
        break;
      case "flip":
        canvas = applyFlip(canvas, op.horizontal);
        break;
      case "resize": {
        const c = applyResize(canvas, op.w * scale, op.h * scale);
        canvas = c;
        break;
      }
      case "blur":
        canvas = applyBlur(canvas, op.sigma * scale);
        break;
      case "mosaic":
        canvas = applyMosaic(canvas, Math.max(1, Math.round(op.block * scale)));
        break;
      default:
        // 像素 op 就地作用，顺序与 Rust 侧一致（顺序不同结果会不同）。
        applyPixelOp(canvas, op);
        break;
    }
  }
  return canvas;
}
