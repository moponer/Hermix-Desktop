// PDF **文档级**工具：合并 / 裁剪 / 水印 / 页码 / 文档信息 / 打开密码。
//
// 分工（别混）：内容流级别的手术（文本替换、页级增删排）留在 Rust 的
// lopdf；**文档级**操作在这里。两边各自导出一个新文件，不做交叉。
//
// 为什么用 pdf-lib 而不是继续在 lopdf 里手搓：合并多文件、页面框、绘制
// 水印与页码都是二进制级别的活，pdf-lib 有成熟实现；自己手写等于把几百
// 行高风险代码塞进产品。pdf-lib 是 MIT，**只在用户真的打开 PDF 工具时
// 才动态加载**，不进首屏包。
//
// 中文字��：标准 14 号字只有拉丁字形，水印若是 ASCII 就用矢量字体
// （体积小、任意缩放清晰）；含中文时改走「浏览器画布 → PNG → 贴进页面」
// ——画布用系统字体，中文正常显示，代价是位图水印（体积略大）。

import type { PDFDocument, PDFPage } from "pdf-lib";

export interface CropTool {
  /** 页码（1 起）；空 = 全部页。 */
  pages: number[];
  /** 四边裁掉的磅数（1/72 英寸）。 */
  left: number;
  right: number;
  top: number;
  bottom: number;
}

export interface WatermarkTool {
  text: string;
  /** 字号（磅）。 */
  fontSize: number;
  /** 不透明度 0..1。 */
  opacity: number;
  /** 角度（度），0 = 水平。 */
  angle: number;
  /** CSS 颜色（#rrggbb）。 */
  color: string;
  /** 页码（1 起）；空 = 全部页。 */
  pages: number[];
}

export interface PageNumberTool {
  /** 页码文本模板：{n} = 当前页码，{total} = 总页数。 */
  format: string;
  /** 起始页码。 */
  startAt: number;
  pages: number[];
}

export interface MetaTool {
  title?: string;
  author?: string;
  subject?: string;
  keywords?: string;
}

export interface PdfToolSet {
  crop?: CropTool;
  watermark?: WatermarkTool;
  pageNumbers?: PageNumberTool;
  meta?: MetaTool;
}

/** 懒加载 pdf-lib：只有真的用文档工具时才进内存。 */
async function lib() {
  return await import("pdf-lib");
}

/** #rrggbb / rgb(...) → pdf-lib 的 rgb()。 */
function parseColor(css: string): { r: number; g: number; b: number } {
  const m = css.trim().match(/^#?([0-9a-f]{6})$/i);
  if (m) {
    const n = parseInt(m[1], 16);
    return { r: ((n >> 16) & 255) / 255, g: ((n >> 8) & 255) / 255, b: (n & 255) / 255 };
  }
  const rgb = css.match(/rgba?\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)/i);
  if (rgb) {
    return {
      r: Math.min(255, Number(rgb[1])) / 255,
      g: Math.min(255, Number(rgb[2])) / 255,
      b: Math.min(255, Number(rgb[3])) / 255,
    };
  }
  return { r: 0.5, g: 0.5, b: 0.5 };
}

/** ASCII 水印用矢量标准字体；含中文走画布位图。 */
function isAscii(text: string): boolean {
  return /^[\x20-\x7e]*$/.test(text);
}

/**
 * 画布渲染文字 → PNG 字节。
 *
 * 用浏览器自己的字体栈（微软雅黑 / Segoe UI），中文与 emoji 都能显示，
 * 代价是位图（不是矢量）。
 */
async function textToPng(text: string, fontSizePx: number, color: string): Promise<Uint8Array> {
  const probe = document.createElement("canvas").getContext("2d");
  if (!probe) throw new Error("画布不可用");
  const font = `${fontSizePx}px "Microsoft YaHei", "PingFang SC", "Segoe UI", sans-serif`;
  probe.font = font;
  const textW = probe.measureText(text).width;
  const padX = Math.ceil(fontSizePx * 0.3);
  const w = Math.max(1, Math.ceil(textW) + padX * 2);
  const h = Math.max(1, Math.ceil(fontSizePx * 1.6));
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("画布不可用");
  ctx.font = font;
  ctx.fillStyle = color;
  ctx.textBaseline = "middle";
  ctx.textAlign = "center";
  ctx.fillText(text, w / 2, h / 2);
  const blob = await new Promise<Blob | null>((res) => canvas.toBlob(res, "image/png"));
  if (!blob) throw new Error("文字水印渲染失败");
  return new Uint8Array(await blob.arrayBuffer());
}

/** 命中的页（1 起页码 → 索引）。空 = 全部。 */
function targets(pages: PDFPage[], wanted: number[] | undefined): PDFPage[] {
  if (!wanted || wanted.length === 0) return pages;
  const set = new Set(wanted);
  return pages.filter((_, i) => set.has(i + 1));
}

/**
 * 合并多份 PDF（按给定顺序）。每份的页依次追加。
 */
export async function mergePdfFiles(list: Uint8Array[]): Promise<Uint8Array> {
  if (list.length === 0) throw new Error("没有可合并的文件");
  const { PDFDocument } = await lib();
  const out = await PDFDocument.create();
  for (const bytes of list) {
    const src = await PDFDocument.load(bytes, { ignoreEncryption: true });
    const copied = await out.copyPages(src, src.getPageIndices());
    for (const p of copied) out.addPage(p);
  }
  return await out.save();
}

/**
 * 对一份 PDF 施加文档级工具，产出新的 PDF 字节。
 */
export async function applyPdfTools(bytes: Uint8Array, tools: PdfToolSet): Promise<Uint8Array> {
  const { PDFDocument, StandardFonts, degrees, rgb } = await lib();
  const doc = await PDFDocument.load(bytes, { ignoreEncryption: true });
  const pages = doc.getPages();

  // ── 裁剪（CropBox）──────────────────────────────────────────────
  if (tools.crop) {
    const { left, right, top, bottom } = tools.crop;
    for (const page of targets(pages, tools.crop.pages)) {
      const box = page.getMediaBox();
      const w = Math.max(1, box.width - left - right);
      const h = Math.max(1, box.height - top - bottom);
      page.setCropBox(box.x + left, box.y + bottom, w, h);
    }
  }

  // ── 水印 ────────────────────────────────────────────────────────
  if (tools.watermark && tools.watermark.text.trim()) {
    const spec = tools.watermark;
    const color = parseColor(spec.color);
    const picked = targets(pages, spec.pages);
    const ascii = isAscii(spec.text);
    // 字号：矢量字体直接用磅；位图走画布时按 96dpi 换算（1px = 0.75pt）。
    const fontPx = Math.max(8, Math.round((spec.fontSize / 0.75) * 1));
    const png = ascii ? null : await textToPng(spec.text, fontPx, spec.color);
    const font = ascii ? await doc.embedFont(StandardFonts.Helvetica) : null;

    for (const page of picked) {
      const box = page.getMediaBox();
      if (font) {
        const tw = font.widthOfTextAtSize(spec.text, spec.fontSize);
        page.drawText(spec.text, {
          x: box.width / 2 - tw / 2,
          y: box.height / 2 - spec.fontSize / 2,
          size: spec.fontSize,
          font,
          color: rgb(color.r, color.g, color.b),
          opacity: Math.min(1, Math.max(0, spec.opacity)),
          rotate: degrees(spec.angle),
        });
      } else if (png) {
        const image = await doc.embedPng(png);
        // CSS 像素 → 磅：0.75。
        const w = image.width * 0.75;
        const h = image.height * 0.75;
        const rad = (spec.angle * Math.PI) / 180;
        // 绕图片中心旋转：把旋转后的中心偏移从页面中心里扣掉。
        const ox =
          box.width / 2 - (w / 2 * Math.cos(rad) - h / 2 * Math.sin(rad));
        const oy =
          box.height / 2 - (w / 2 * Math.sin(rad) + h / 2 * Math.cos(rad));
        page.drawImage(image, {
          x: ox,
          y: oy,
          width: w,
          height: h,
          rotate: degrees(spec.angle),
          opacity: Math.min(1, Math.max(0, spec.opacity)),
        });
      }
    }
  }

  // ── 页码 ────────────────────────────────────────────────────────
  if (tools.pageNumbers) {
    const spec = tools.pageNumbers;
    const font = await doc.embedFont(StandardFonts.Helvetica);
    const picked = targets(pages, spec.pages);
    const total = picked.length;
    picked.forEach((page, i) => {
      const label = spec.format
        .replaceAll("{n}", String(spec.startAt + i))
        .replaceAll("{total}", String(total));
      if (!label) return;
      const box = page.getMediaBox();
      const size = 10;
      const tw = font.widthOfTextAtSize(label, size);
      page.drawText(label, {
        x: box.width / 2 - tw / 2,
        y: 24,
        size,
        font,
        color: rgb(0.15, 0.15, 0.15),
      });
    });
  }

  // ── 文档信息 ────────────────────────────────────────────────────
  if (tools.meta) {
    const m = tools.meta;
    if (m.title !== undefined) doc.setTitle(m.title);
    if (m.author !== undefined) doc.setAuthor(m.author);
    if (m.subject !== undefined) doc.setSubject(m.subject);
    if (m.keywords !== undefined) doc.setKeywords([m.keywords]);
  }

  // ── 加密 ──────────────────────────────────────────────────────
  // 本轮不做：pdf-lib 1.17 与 lopdf 0.34 都只能**读**加密 PDF，不能写
  // （写加密要自己实现 RC4/AES 的密钥派生与逐对象加密）。宁可不做，
  // 也不做出「以为加密了其实没加密」的假象。

  return await doc.save();
}

/** 读出 PDF 的页数与元信息（给 UI 显示用）。 */
export async function readPdfMeta(bytes: Uint8Array): Promise<{
  pages: number;
  title?: string;
  author?: string;
  subject?: string;
  keywords?: string;
}> {
  const { PDFDocument } = await lib();
  const doc = await PDFDocument.load(bytes, { ignoreEncryption: true });
  return {
    pages: doc.getPageCount(),
    title: doc.getTitle(),
    author: doc.getAuthor(),
    subject: doc.getSubject(),
    keywords: doc.getKeywords(),
  };
}

export type { PDFDocument };
