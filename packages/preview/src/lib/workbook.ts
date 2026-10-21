// 电子表格保真模型 —— 把 xlsx / xls / csv 统一成「带样式的网格」。
//
// 为什么不用 SheetJS：实测（见 docs/notes/implemented/feature/2026-10-13-
// preview-rendering-fidelity.md）SheetJS 社区版读 xlsx 时 `cellStyles: true`
// 只能拿回 **fill**，font / border / alignment / numFmt 全部丢失，也就是
// 「加粗、颜色、边框、居中、数字格式」这些正是用户最先看到的格式。换成
// xlsx-js-style 结果完全一样（同一份 SheetJS 内核）。ExcelJS 是唯一能把
// 这些全读回来的开源实现，所以 .xlsx/.xlsm 走 ExcelJS。
//
// 三种来源各有物理边界，不是重复代码：
//   xlsx/xlsm → ExcelJS：唯一能读 OOXML 样式的引擎
//   xls       → SheetJS：ExcelJS 不支持 BIFF 老二进制格式，SheetJS 支持，
//                      但 BIFF 样式读不回来（老格式本身也没有 xlsx 那套样式表）
//   csv/tsv   → PapaParse：文本，不需要任何工作簿解析
//
// 两个引擎都是动态 import：打开 .md / .docx / 代码文件时不该为一个
// 表格引擎付 900 KB 下载成本。
//
// 这一层刻意**不 import 引擎的类型**：翻译规则（颜色/边框/对齐/单位）是
// 纯函数，与哪个引擎读出来的数据无关；把它们绑死在 ExcelJS 的类型上，
// 引擎一换类型就全线飘。下面的结构类型只描述"我们真正会读的字段"。

import type { CSSProperties } from "react";

/**
 * Excel 的颜色写法。ExcelJS 自己的 `Color` 把 `argb` 与 `theme` 都标成必填，
 * 可运行时三者只会有一个，用它做参数类型既表达不了真实数据也不必要。
 */
export type ColorLike = {
  argb?: string;
  theme?: number;
  tint?: number;
  indexed?: number;
};

type FontLike = {
  bold?: boolean;
  italic?: boolean;
  strike?: boolean;
  underline?: boolean;
  color?: ColorLike;
  size?: number;
  name?: string;
};

type FillLike = {
  type?: string;
  pattern?: string;
  fgColor?: ColorLike;
  bgColor?: ColorLike;
  /** 渐变填充的色标（ExcelJS 把 gradientFill 存成 stops 数组）。 */
  stops?: Array<{ position?: number; color?: ColorLike }>;
};

type BorderLike = { style?: string; color?: ColorLike };

type BorderSetLike = {
  top?: BorderLike;
  right?: BorderLike;
  bottom?: BorderLike;
  left?: BorderLike;
};

type AlignLike = {
  horizontal?: string;
  vertical?: string;
  wrapText?: boolean;
  indent?: number;
  textRotation?: number;
};

/** 单个格子的边框边。`style` 是 CSS border 简写，`color` 已解析成 #RRGGBB。 */
export type BorderEdge = { css: string; color?: string } | null;

export type CellBorderSet = {
  top?: BorderEdge;
  right?: BorderEdge;
  bottom?: BorderEdge;
  left?: BorderEdge;
};

/** 一个格子的样式，已经从引擎结构翻译成可直接铺到 DOM 上的量。 */
export type CellStyle = {
  bg?: string;
  color?: string;
  bold?: boolean;
  italic?: boolean;
  underline?: boolean;
  strike?: boolean;
  /** 磅值，与源文件一致。 */
  size?: number;
  font?: string;
  align?: "left" | "center" | "right" | "justify";
  valign?: "top" | "middle" | "bottom";
  wrap?: boolean;
  indent?: number;
  /** Excel 的竖排文字：0-90 顺时针，91-180 逆时针。 */
  rotation?: number;
  border?: CellBorderSet;
  /** 源文件里的数字格式串，鼠标悬停可见（显示值已经按它格式化过了）。 */
  numFmt?: string;
};

export type CellModel = {
  /** 屏幕显示值：已按 numFmt 格式化，日期/百分比/千分位与 Excel 一致。 */
  text: string;
  style?: CellStyle;
  /** 公式单元格带这个，悬停显示原公式。 */
  formula?: string;
  /** 超链接目标。 */
  href?: string;
};

export type SheetMerge = {
  row: number;
  col: number;
  rowSpan: number;
  colSpan: number;
};

export type SheetModel = {
  name: string;
  rowCount: number;
  colCount: number;
  /** rows[r][c]；空格子是 null，合并区的从属格也为 null（由主格承担）。 */
  rows: Array<Array<CellModel | null>>;
  merges: SheetMerge[];
  /** 像素列宽，null = 用默认宽。 */
  colWidths: Array<number | null>;
  /** 像素行高，null = 默认行高。 */
  rowHeights: Array<number | null>;
  /** 冻结窗格的分行列数，null = 不冻结。 */
  frozen: { x: number; y: number } | null;
  hiddenRows: Set<number>;
  hiddenCols: Set<number>;
};

export type WorkbookModel = {
  sheets: SheetModel[];
};

/** 有样式的来源（xlsx）vs 没有样式的来源（xls / csv）。UI 据此如实标注。 */
export type FidelityLevel = "styled" | "plain";

export type ParsedWorkbook = WorkbookModel & { fidelity: FidelityLevel };

/** 单表解析上限。超出部分不渲染但如实告知，不静默截断。 */
export const ROW_LIMIT = 20000;
export const COL_LIMIT = 512;

/** 表格默认字号（磅），与 Excel 默认工作簿一致；无显式字号的格子用它。 */
export const DEFAULT_FONT_PT = 11;

/** Excel 默认行高 15pt = 20px；没有显式行高时用它。 */
export const DEFAULT_ROW_H = 20;

/**
 * 纸面墨色：表格是"一张白纸"，正文默认黑字。
 *
 * 界面主题（尤其深色）不该渗进表格里：深色界面 + 白纸 + 浅色字 = 白底
 * 浅字，内容直接看不见。表格有自己的颜色世界，跟 Office 一样。
 */
export const SHEET_INK = "#000000";

/**
 * 给定格子底色，选一个看得清的墨色。
 *
 * Excel 里字体色总是显式写死的，缺省只出现在"只有底色、没有字色"的
 * 格子（表头底色行最常见）。这时若沿用界面主题的浅色字，落在黄色 /
 * 浅色底上就等于隐形 —— 按底色亮度决定黑白才是稳的。
 */
export function contrastInk(bg: string | undefined): string {
  if (!bg) return SHEET_INK;
  const m = /^#([0-9a-f]{6})$/i.exec(bg);
  if (!m) return SHEET_INK;
  const num = parseInt(m[1], 16);
  const r = ((num >> 16) & 255) / 255;
  const g = ((num >> 8) & 255) / 255;
  const b = (num & 255) / 255;
  // 感知亮度（sRGB → 线性 → 加权）
  const lin = (c: number) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const lum = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  return lum > 0.45 ? SHEET_INK : "#FFFFFF";
}

/* ══════════════════════════════════════════════════════════════════
   颜色：argb / theme(+tint) / indexed 三种写法 → #RRGGBB
   ══════════════════════════════════════════════════════════════════ */

/**
 * Office 标准主题色板。索引顺序被 styles.xml 重排过：0/1 是 lt1/dk1
 * （clrScheme 里写的却是 dk1 在前），2/3 是 lt2/dk2，之后 accent1-6、
 * hlink、folHlink。文件自带 theme1.xml 时优先用文件自己的，这里兜底。
 */
const OFFICE_THEME = [
  "FFFFFF", "000000", "E7E6E6", "44546A",
  "4472C4", "ED7D31", "A5A5A5", "FFC000", "5B9BD5", "70AD47",
  "0563C1", "954F72",
] as const;

/** OOXML 规范里的 64 色 indexed 调色板（写死的一组常量，不是自造协议）。 */
const INDEXED_PALETTE =
  "000000,FFFFFF,FF0000,00FF00,0000FF,FFFF00,FF00FF,00FFFF," +
  "000000,FFFFFF,FF0000,00FF00,0000FF,FFFF00,FF00FF,00FFFF," +
  "800000,008000,000080,808000,800080,008080,C0C0C0,808080," +
  "9999FF,993366,FFFFCC,CCFFFF,660066,FF8080,0066CC,CCCCFF," +
  "000080,FF00FF,FFFF00,00FFFF,800080,800000,008080,0000FF," +
  "00CCFF,CCFFFF,CCFFCC,FFFF99,99CCFF,FF99CC,CC99FF,FFCC99," +
  "3366FF,33CCCC,99CC00,FFCC00,FF9900,FF6600,666699,969696," +
  "003366,339966,003300,333300,993300,993366,333399,333333";

function clamp255(n: number): number {
  return n < 0 ? 0 : n > 255 ? 255 : Math.round(n);
}

function toHex(r: number, g: number, b: number): string {
  // 大写：与 resolveColor / themeColor 的输出一致（同一个颜色不能有两种大小写）。
  return `#${[r, g, b]
    .map((v) => clamp255(v).toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase()}`;
}

/* ── HSL：Excel 的 tint 走的是 HSL 亮度通道，不是 sRGB 逐通道 ────── */

function rgbToHsl(r: number, g: number, b: number): [number, number, number] {
  const rr = r / 255;
  const gg = g / 255;
  const bb = b / 255;
  const max = Math.max(rr, gg, bb);
  const min = Math.min(rr, gg, bb);
  const l = (max + min) / 2;
  const d = max - min;
  if (d === 0) return [0, 0, l];
  const s = l > 0.5 ? d / (2 - max - min) : d / (max + min);
  let h: number;
  if (max === rr) h = ((gg - bb) / d) % 6;
  else if (max === gg) h = (bb - rr) / d + 2;
  else h = (rr - gg) / d + 4;
  h *= 60;
  if (h < 0) h += 360;
  return [h, s, l];
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const hp = (((h % 360) + 360) % 360) / 60;
  const x = c * (1 - Math.abs((hp % 2) - 1));
  let r = 0;
  let g = 0;
  let b = 0;
  if (hp < 1) [r, g, b] = [c, x, 0];
  else if (hp < 2) [r, g, b] = [x, c, 0];
  else if (hp < 3) [r, g, b] = [0, c, x];
  else if (hp < 4) [r, g, b] = [0, x, c];
  else if (hp < 5) [r, g, b] = [x, 0, c];
  else [r, g, b] = [c, 0, x];
  const m = l - c / 2;
  return [(r + m) * 255, (g + m) * 255, (b + m) * 255];
}

/**
 * ECMA-376 的 tint：在 **HSL 的亮度通道**上改，色相与饱和度不动。
 *
 * 这一条不能简化成"逐通道线性插值"——那是上一版的做法，对纯灰色碰巧
 * 对，对带色主题色就是错的：accent1 (#4472C4) 加 tint 0.4，逐通道插值
 * 得到 #8FAADC，而 Excel 真正画出来的是 #8EAADB；tint -0.25 逐通道得到
 * #335693，Excel 是 #2F5597。用户看到的"颜色发闷/不对味"就是这里。
 *
 * 公式（ECMA-376 §18.8.27）：
 *   tint < 0：L' = L × (1 + tint)
 *   tint > 0：L' = L × (1 - tint) + tint
 */
export function applyTint(hex: string, tint: number): string {
  if (!tint) return hex;
  const num = parseInt(hex.slice(1), 16);
  const [h, s, l] = rgbToHsl((num >> 16) & 255, (num >> 8) & 255, num & 255);
  const nl = tint < 0 ? l * (1 + tint) : l * (1 - tint) + tint;
  const [r, g, b] = hslToRgb(h, s, Math.min(1, Math.max(0, nl)));
  return toHex(r, g, b);
}

/**
 * 主题索引 → #RRGGBB（带 tint）。越界返回 undefined，不猜。
 *
 * `theme` 是**文件自带**的色板；某个索引在文件里缺失时，逐个回退到
 * Office 默认色板——不是整张色板一起作废（上一版要求 12 个色全齐，
 * 少一个就把整份主题丢掉，所有主题色都退成默认蓝，是"颜色大面积不对"
 * 的直接原因）。
 */
export function themeColor(
  index: number,
  tint: number,
  theme?: readonly (string | undefined)[],
): string | undefined {
  const raw = (theme && theme[index]) || OFFICE_THEME[index];
  if (!raw) return undefined;
  const base = raw.startsWith("#") ? raw.slice(1) : raw;
  if (!/^[0-9a-fA-F]{6}$/.test(base)) return undefined;
  return applyTint(`#${base.toUpperCase()}`, tint);
}

/**
 * Excel 的颜色写法。**刻意自己声明**而不是用 ExcelJS 的 `Color`：那个类型
 * 把 `argb` 与 `theme` 都标成必填，可运行时三者只会有一个，交给它做参数
 * 类型既表达不了真实数据，又逼着每个纯函数都依赖引擎的类型。
 */
export function resolveColor(
  color: ColorLike | undefined,
  theme?: readonly (string | undefined)[],
): string | undefined {
  if (!color) return undefined;
  const c = color;
  if (typeof c.argb === "string") {
    // ARGB → RGB。alpha 全 FF 时正常；00 是"自动"，直接忽略透明度。
    const argb = c.argb.length === 8 ? c.argb.slice(2) : c.argb;
    if (!/^[0-9a-fA-F]{6}$/.test(argb)) return undefined;
    return `#${argb.toUpperCase()}`;
  }
  if (typeof c.theme === "number") return themeColor(c.theme, c.tint ?? 0, theme);
  if (typeof c.indexed === "number") {
    const list = INDEXED_PALETTE.split(",");
    const hex = list[c.indexed];
    return hex ? `#${hex.toUpperCase()}` : undefined;
  }
  return undefined;
}

/* ══════════════════════════════════════════════════════════════════
   边框 / 对齐 / 单位换算
   ══════════════════════════════════════════════════════════════════ */

/** OOXML 边框样式名 → CSS border 简写。线宽按 Excel 的视觉粗细给。 */
const BORDER_CSS: Record<string, string> = {
  hair: "1px solid",
  thin: "1px solid",
  medium: "2px solid",
  thick: "3px solid",
  dotted: "1px dotted",
  dashed: "1px dashed",
  dashDot: "1px dashed",
  dashDotDot: "1px dotted",
  mediumDashed: "2px dashed",
  mediumDashDot: "2px dashed",
  mediumDashDotDot: "2px dotted",
  slantDashDot: "3px dashed",
  double: "3px double",
};

export function borderEdge(
  side: BorderLike | undefined,
  theme?: readonly (string | undefined)[],
): BorderEdge {
  if (!side || !side.style) return null;
  const css = BORDER_CSS[side.style];
  if (!css) return null;
  return { css, color: resolveColor(side.color, theme) };
}

function mapAlign(h: string | undefined): CellStyle["align"] | undefined {
  switch (h) {
    case "center":
      return "center";
    case "right":
    case "distributed":
      return "right";
    case "justify":
      return "justify";
    case "left":
      return "left";
    default:
      // fill / centerContinuous / undefined：Excel 用 fill 表示"按内容左对齐"
      return undefined;
  }
}

/**
 * Excel 竖排角度 → CSS。只处理真正的竖排（90°±），其余角度 Excel 本身
 * 也只支持这几个，斜排不是 OOXML 的能力。
 */
function mapRotation(deg: number | undefined): number | undefined {
  if (deg === undefined || deg === 0) return undefined;
  const abs = Math.abs(deg);
  if (abs === 90 || abs === 91) return deg > 0 ? 90 : -90;
  if (abs === 180) return 180;
  return undefined;
}

/**
 * Excel 列宽单位（以默认字体最大数字宽度为 1）→ 像素。
 * 公式来自 Excel 自己的换算：`px = 字符宽 × MDW + 5`，Calibri 11 的
 * MDW = 7px。少这 5px 的话列会比 Excel 窄一截，长文本被截断。
 */
export function colWidthPx(width: number | undefined): number | null {
  if (width == null || !Number.isFinite(width) || width <= 0) return null;
  return Math.round(width * 7 + 5);
}

/** 行高单位是磅 → 像素（96dpi）。 */
export function rowHeightPx(points: number | undefined): number | null {
  if (points == null || !Number.isFinite(points) || points <= 0) return null;
  return Math.round((points * 96) / 72);
}

/** 0 基列号 → Excel 列标（A / B / … / Z / AA）。故意**不**在此文件里
 *  实现 — 否则外部静态 import 这个函数会拖整个 workbook.ts 进主 chunk，
 * 把 exceljs 也拽进来。统一从 `lib/localAssets` 导出。 */
export { colName } from "./localAssets";

/** "A1:B2" → 0 基的 {row,col,rowSpan,colSpan}。 */
export function parseRange(
  ref: string,
): SheetMerge | null {
  const m = /^([A-Z]+)(\d+)(?::([A-Z]+)(\d+))?$/.exec(ref.toUpperCase());
  if (!m) return null;
  const toCol = (s: string) => {
    let n = 0;
    for (const ch of s) n = n * 26 + (ch.charCodeAt(0) - 64);
    return n - 1;
  };
  const r1 = Number(m[2]) - 1;
  const c1 = toCol(m[1]);
  const r2 = m[4] ? Number(m[4]) - 1 : r1;
  const c2 = m[3] ? toCol(m[3]) : c1;
  if (r1 < 0 || c1 < 0) return null;
  return {
    row: r1,
    col: c1,
    rowSpan: r2 - r1 + 1,
    colSpan: c2 - c1 + 1,
  };
}

/* ══════════════════════════════════════════════════════════════════
   ExcelJS → SheetModel
   ══════════════════════════════════════════════════════════════════ */

type ExcelCell = {
  address?: string;
  value: unknown;
  text?: string;
  formula?: string;
  error?: string;
  hyperlink?: string;
  numFmt?: string;
  font?: FontLike;
  fill?: FillLike;
  alignment?: AlignLike;
  border?: BorderSetLike;
  master?: { address?: string };
};

function cellStyle(
  cell: ExcelCell,
  theme: readonly (string | undefined)[] | undefined,
): CellStyle | undefined {
  const style: CellStyle = {};
  const font = cell.font;
  if (font) {
    if (font.bold) style.bold = true;
    if (font.italic) style.italic = true;
    if (font.strike) style.strike = true;
    if (font.underline) style.underline = true;
    const color = resolveColor(font.color, theme);
    if (color) style.color = color;
    if (typeof font.size === "number" && font.size > 0) style.size = font.size;
    if (font.name) style.font = font.name;
  }

  // 只有 solid 实心填充才落成背景色。OOXML 里 patternType="none" 与
  // "gray125"（Excel 的"无填充"默认值）表示透明，不该画任何底。
  const fill = cell.fill;
  if (fill) {
    if (fill.type === "pattern" && fill.pattern === "solid") {
      const bg = resolveColor(fill.fgColor, theme) ?? resolveColor(fill.bgColor, theme);
      if (bg) style.bg = bg;
    } else if (fill.type === "gradient" && fill.stops?.length) {
      // 渐变填充：CSS 没有渐变单元格背景，取各色标的平均色近似。
      // 不处理的话这类格子会退回白底 —— 白底 + 白字 = "表格内容看不见"。
      const rgb = fill.stops
        .map((s) => resolveColor(s.color, theme))
        .filter((c): c is string => !!c)
        .map((c) => parseInt(c.slice(1), 16));
      if (rgb.length) {
        let r = 0;
        let g = 0;
        let b = 0;
        for (const v of rgb) {
          r += (v >> 16) & 255;
          g += (v >> 8) & 255;
          b += v & 255;
        }
        style.bg = toHex(r / rgb.length, g / rgb.length, b / rgb.length);
      }
    }
  }

  const align = cell.alignment;
  if (align) {
    const h = mapAlign(align.horizontal);
    if (h) style.align = h;
    if (align.vertical === "top") style.valign = "top";
    else if (align.vertical === "bottom") style.valign = "bottom";
    else if (align.vertical === "middle") style.valign = "middle";
    if (align.wrapText) style.wrap = true;
    if (typeof align.indent === "number" && align.indent > 0) style.indent = align.indent;
    const rot = mapRotation(align.textRotation);
    if (rot !== undefined) style.rotation = rot;
  }

  const border = cell.border;
  if (border) {
    const set: CellBorderSet = {};
    const top = borderEdge(border.top, theme);
    const right = borderEdge(border.right, theme);
    const bottom = borderEdge(border.bottom, theme);
    const left = borderEdge(border.left, theme);
    if (top) set.top = top;
    if (right) set.right = right;
    if (bottom) set.bottom = bottom;
    if (left) set.left = left;
    if (set.top || set.right || set.bottom || set.left) style.border = set;
  }

  if (cell.numFmt && cell.numFmt !== "General") style.numFmt = cell.numFmt;
  return Object.keys(style).length ? style : undefined;
}

/**
 * ExcelJS 不按 numFmt 渲染显示值（`cell.text` 对 1234.5 就是 "1234.5"），
 * 所以格式化这一步得自己做。SheetJS 的 SSF 是 Excel 数字格式的完整实现，
 * 而且它已经是本项目的依赖（.xls 走的就是它）——复用现成引擎，不引第二套
 * 格式化实现，也就不存在两套日期口径打架。
 *
 * 按需加载：整个工作簿没有一处非 General 格式时，永远不碰这个模块。
 */
type Ssf = { format(fmt: string, value: number | string): string };
let ssfCache: Ssf | null | undefined;

async function loadSsf(): Promise<Ssf | null> {
  if (ssfCache !== undefined) return ssfCache;
  try {
    const mod = (await import("xlsx")) as { SSF?: Ssf; default?: { SSF?: Ssf } };
    ssfCache = mod.SSF ?? mod.default?.SSF ?? null;
  } catch {
    ssfCache = null;
  }
  return ssfCache;
}

/** JS Date → Excel 序列号（1900 日期系统）。1899-12-30 是 Excel 的第 0 天。 */
function dateToSerial(d: Date): number {
  const ms = Date.UTC(
    d.getFullYear(),
    d.getMonth(),
    d.getDate(),
    d.getHours(),
    d.getMinutes(),
    d.getSeconds(),
  );
  return (ms - Date.UTC(1899, 11, 30)) / 86400000;
}

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/**
 * 单元格显示值。数值与日期按 numFmt 走 SSF（千分位、百分比、日期各按源
 * 文件的格式串）；其余（富文本、超链接、错误值、公式结果）用引擎自己的
 * text —— 那些结构 SSF 不认，交给引擎更可靠。
 */
function displayText(cell: ExcelCell, ssf: Ssf | null): string {
  if (cell.error) return cell.error;
  const value = cell.value;
  const numFmt = cell.numFmt;
  if (ssf && numFmt && numFmt !== "General") {
    if (value instanceof Date) {
      try {
        return ssf.format(numFmt, dateToSerial(value));
      } catch {
        // 坏格式串退回下面的通用渲染
      }
    } else if (typeof value === "number") {
      try {
        return ssf.format(numFmt, value);
      } catch {
        // 同上
      }
    }
  }
  // 没有格式串的日期：Excel 的 General 会显示成日期而不是 Date.toString()
  if (value instanceof Date) {
    const d = value;
    const day = `${d.getFullYear()}-${pad2(d.getMonth() + 1)}-${pad2(d.getDate())}`;
    const hasTime = d.getHours() || d.getMinutes() || d.getSeconds();
    return hasTime ? `${day} ${pad2(d.getHours())}:${pad2(d.getMinutes())}` : day;
  }
  if (typeof cell.text === "string") return cell.text;
  if (value == null) return "";
  return String(value);
}

/**
 * 从工作簿里抽主题色板；抽不到就用 Office 默认值。
 *
 * 逐个索引回退：文件里的主题少写了 hlink / folHlink 之类时，只有那几个
 * 索引退到默认值，其余仍然用文件自己的色板。上一版是"12 个色必须齐全，
 * 否则整张丢弃"，结果一个不常见的缺项就把全文的主题色打成默认蓝。
 */
function readTheme(wb: unknown): (string | undefined)[] | undefined {
  // ExcelJS 把 xl/theme/theme1.xml 原样存成字符串，没有解析成对象。
  const themes = (wb as { model?: { themes?: Record<string, string> } }).model?.themes;
  if (!themes) return undefined;
  // 键名通常是 theme1，但规范允许 theme2…；按前缀找而不是写死 theme1。
  const key = Object.keys(themes).find((k) => /^theme\d+$/.test(k));
  const xml = key ? themes[key] : undefined;
  if (!xml || typeof DOMParser === "undefined") return undefined;
  try {
    const doc = new DOMParser().parseFromString(xml, "application/xml");
    const scheme = doc.querySelector("clrScheme");
    if (!scheme) return undefined;
    const hex = (name: string) => {
      const node = scheme.querySelector(name);
      const srgb = node?.querySelector("srgbClr");
      const sys = node?.querySelector("sysClr");
      const val = srgb?.getAttribute("val") ?? sys?.getAttribute("lastClr");
      return typeof val === "string" ? val.toUpperCase() : undefined;
    };
    // clrScheme 的书写顺序是 dk1/lt1/lt2/dk2…，但 styles.xml 的 theme
    // 索引把前两位换成了 lt1/dk1，所以这里按索引表重排。
    const ordered: (string | undefined)[] = [
      hex("lt1") ?? hex("dk1"),
      hex("dk1") ?? hex("lt1"),
      hex("lt2"),
      hex("dk2"),
      ...[1, 2, 3, 4, 5, 6].map((i) => hex(`accent${i}`)),
      hex("hlink"),
      hex("folHlink"),
    ];
    return ordered.some((v) => typeof v === "string") ? ordered : undefined;
  } catch {
    return undefined;
  }
}

/** 我们真正会读的工作表字段。`_merges` 是 ExcelJS 的私有字段，读取时兜底。 */
type SheetSource = {
  name: string;
  workbook: unknown;
  rowCount: number;
  columnCount: number;
  model?: { merges?: string[] };
  views?: unknown[];
  _merges?: Record<string, unknown>;
  getRow(n: number): { hidden?: boolean; height?: number; getCell(n: number): unknown };
  getColumn(n: number): { hidden?: boolean; width?: number };
};

async function sheetFromWorksheet(ws: SheetSource): Promise<SheetModel> {
  const wb = ws.workbook as unknown;
  const theme = readTheme(wb);
  const rowCount = Math.min(ws.rowCount, ROW_LIMIT);
  const colCount = Math.min(Math.max(ws.columnCount, 1), COL_LIMIT);

  // 合并区：从属格要留空，否则同一段文字会在每个被覆盖的格子里重复出现。
  const covered = new Set<string>();
  const merges: SheetMerge[] = [];
  const mergeRefs: string[] = [
    ...(ws.model?.merges ?? []),
    ...Object.keys(ws._merges ?? {}),
  ];
  for (const ref of mergeRefs) {
    const m = parseRange(ref);
    if (!m) continue;
    merges.push(m);
    for (let r = m.row; r < m.row + m.rowSpan; r++) {
      for (let c = m.col; c < m.col + m.colSpan; c++) {
        if (r === m.row && c === m.col) continue;
        covered.add(`${r}:${c}`);
      }
    }
  }

  const rows: Array<Array<CellModel | null>> = [];
  const rowHeights: Array<number | null> = [];
  const hiddenRows = new Set<number>();
  let ssf: Ssf | null | undefined;

  for (let r = 1; r <= rowCount; r++) {
    const row = ws.getRow(r);
    if (row.hidden) hiddenRows.add(r - 1);
    const explicitHeight = rowHeightPx(row.height as number | undefined);
    /** 本行最大字号（像素）——行高要装得下它，否则大字被裁掉。 */
    let maxFontPx = 0;

    const cells: Array<CellModel | null> = [];
    for (let c = 1; c <= colCount; c++) {
      if (covered.has(`${r - 1}:${c - 1}`)) {
        cells.push(null);
        continue;
      }
      const cell = row.getCell(c) as unknown as ExcelCell;
      const merged = cell.master && cell.master.address !== cell.address;
      if (merged) {
        // ExcelJS 会把从属格的 master 指回主格；这里一律跳过。
        cells.push(null);
        continue;
      }
      // 只在真的遇到非 General 格式串时才去加载格式化引擎。
      if (ssf === undefined && cell.numFmt && cell.numFmt !== "General") {
        ssf = await loadSsf();
      }
      const text = displayText(cell, ssf ?? null);
      const style = cellStyle(cell, theme);
      const fontPx = ((style?.size ?? DEFAULT_FONT_PT) * 96) / 72;
      if (fontPx > maxFontPx) maxFontPx = fontPx;
      const hasValue = text !== "" || cell.formula || cell.hyperlink;
      if (!hasValue && !style) {
        cells.push(null);
        continue;
      }
      const model: CellModel = { text };
      if (style) model.style = style;
      if (cell.formula) model.formula = cell.formula;
      if (cell.hyperlink) model.href = cell.hyperlink;
      cells.push(model);
    }
    rows.push(cells);
    // 行高 = 源文件行高与"装得下本行最大字号"的高度取大。
    //
    // 只用源文件行高的话，标题行（14pt 粗体）配默认 20px 行高会被裁掉
    // 半个字 —— 表现就是"表格内容看不见"。Excel 自己也会把行撑高，所以这
    // 不是改样式，是补上 Excel 的自动行高。
    const needed = maxFontPx ? Math.ceil(maxFontPx * 1.15) + 3 : 0;
    const height = Math.max(explicitHeight ?? 0, needed);
    rowHeights.push(height || null);
  }

  const colWidths: Array<number | null> = [];
  const hiddenCols = new Set<number>();
  for (let c = 1; c <= colCount; c++) {
    const col = ws.getColumn(c);
    if (col.hidden) hiddenCols.add(c - 1);
    colWidths.push(colWidthPx(col.width as number | undefined));
  }

  // ExcelJS 的 `views[].state` 类型里只写了 "normal" / "split"，但 OOXML
  // 实际还会写 "frozen" / "frozenSplit" —— 运行时读得到，类型里没有。
  const view = (ws.views ?? [])[0] as
    | { state?: string; xSplit?: number; ySplit?: number }
    | undefined;
  const frozen =
    view && (view.state === "frozen" || view.state === "frozenSplit")
      ? { x: view.xSplit ?? 0, y: view.ySplit ?? 0 }
      : null;

  return {
    name: ws.name,
    rowCount,
    colCount,
    rows,
    merges,
    colWidths,
    rowHeights,
    frozen: frozen && (frozen.x > 0 || frozen.y > 0) ? frozen : null,
    hiddenRows,
    hiddenCols,
  };
}

/** 读 .xlsx / .xlsm。样式、合并、列宽、行高、冻结窗格、隐藏行列全部保留。 */
export async function parseXlsx(bytes: Uint8Array): Promise<ParsedWorkbook> {
  const { default: ExcelJS } = await import("exceljs");
  const wb = new ExcelJS.Workbook();
  // ExcelJS 会拿走 ArrayBuffer 的所有权，传副本以免 payload 变成 detached。
  await wb.xlsx.load(new Uint8Array(bytes).buffer);
  const sheets = await Promise.all(wb.worksheets.map(sheetFromWorksheet));
  if (sheets.length === 0) throw new Error("工作簿里没有可读的 Sheet");
  return { sheets, fidelity: "styled" };
}

/**
 * 读老二进制 .xls。ExcelJS 不支持 BIFF，这里是唯一能读的引擎；
 * 代价是 BIFF 的样式表读不回来，所以 fidelity 标 plain —— UI 会如实说明，
 * 而不是渲染一份没有格式的表然后假装"已还原"。
 */
export async function parseXls(bytes: Uint8Array): Promise<ParsedWorkbook> {
  const XLSX = await import("xlsx");
  const wb = XLSX.read(new Uint8Array(bytes), { type: "array", cellDates: true });
  const sheets = wb.SheetNames.map((name) => {
    const ws = wb.Sheets[name];
    const aoa = XLSX.utils.sheet_to_json<unknown[]>(ws, {
      header: 1,
      raw: false,
      defval: "",
    });
    const colCount = Math.min(
      Math.max(1, aoa.reduce((m, r) => Math.max(m, r.length), 0)),
      COL_LIMIT,
    );
    const rows = aoa.slice(0, ROW_LIMIT).map((r) => {
      const cells: Array<CellModel | null> = [];
      for (let c = 0; c < colCount; c++) {
        const v = r[c];
        cells.push(v === undefined || v === null || v === "" ? null : { text: String(v) });
      }
      return cells;
    });
    return {
      name,
      rowCount: rows.length,
      colCount,
      rows,
      merges: [],
      colWidths: new Array<number | null>(colCount).fill(null),
      rowHeights: rows.map(() => null),
      frozen: null,
      hiddenRows: new Set<number>(),
      hiddenCols: new Set<number>(),
    } satisfies SheetModel;
  });
  if (sheets.length === 0) throw new Error("工作簿里没有可读的 Sheet");
  return { sheets, fidelity: "plain" };
}

/** 读 csv / tsv。分隔符交给 PapaParse 自适应，制表符文件不另开分支。 */
export async function parseDelimited(bytes: Uint8Array): Promise<ParsedWorkbook> {
  const Papa = (await import("papaparse")).default;
  const text = new TextDecoder("utf-8", { fatal: false })
    .decode(bytes)
    .replace(/^﻿/, "");
  const result = Papa.parse<string[]>(text, {
    delimiter: "",
    skipEmptyLines: "greedy",
  });
  const aoa = result.data;
  const colCount = Math.min(
    Math.max(1, aoa.reduce((m, r) => Math.max(m, r.length), 0)),
    COL_LIMIT,
  );
  const rows = aoa.slice(0, ROW_LIMIT).map((r) => {
    const cells: Array<CellModel | null> = [];
    for (let c = 0; c < colCount; c++) {
      const v = r[c];
      cells.push(v === undefined || v === null || v === "" ? null : { text: String(v) });
    }
    return cells;
  });
  return {
    sheets: [
      {
        name: "CSV",
        rowCount: rows.length,
        colCount,
        rows,
        merges: [],
        colWidths: new Array<number | null>(colCount).fill(null),
        rowHeights: rows.map(() => null),
        frozen: null,
        hiddenRows: new Set<number>(),
        hiddenCols: new Set<number>(),
      },
    ],
    fidelity: "plain",
  };
}

/** 把一个 SheetModel 导出成 CSV（走 PapaParse 的转义规则）。 */
export async function sheetToCsv(sheet: SheetModel): Promise<string> {
  const Papa = (await import("papaparse")).default;
  return Papa.unparse(
    sheet.rows.map((row) => row.map((c) => c?.text ?? "")),
    { newline: "\r\n" },
  );
}

/* ── 单元格样式 → React 内联样式 ─────────────────────────────────────
   住在 workbook.ts 而不是 SpreadsheetView 里：viewer 的**可编辑**表格
   （apps/viewer/src/document/SheetEditor.tsx）也要同一份映射，两边各写
   一份必然漂移——底色、字色、边框少一处就是"编辑态和预览态长得不一样"。 */

function borderCss(e: { css: string; color?: string } | null | undefined): string | undefined {
  if (!e) return undefined;
  return e.color ? `${e.css} ${e.color}` : e.css;
}

/** CellStyle → 可直接铺到 DOM 上的内联样式。 */
export function styleToCss(style: CellStyle | undefined): CSSProperties {
  if (!style) return {};
  const css: CSSProperties = {};
  if (style.bg) css.backgroundColor = style.bg;
  if (style.color) css.color = style.color;
  if (style.bold) css.fontWeight = 700;
  if (style.italic) css.fontStyle = "italic";
  const deco = style.underline
    ? style.strike
      ? "underline line-through"
      : "underline"
    : style.strike
      ? "line-through"
      : undefined;
  if (deco) css.textDecoration = deco;
  if (style.size) css.fontSize = `${(style.size * 96) / 72}px`;
  if (style.font) css.fontFamily = `"${style.font}", var(--font-sans)`;
  if (style.align) css.textAlign = style.align;
  if (style.valign) css.verticalAlign = style.valign;
  css.whiteSpace = style.wrap ? "pre-wrap" : "nowrap";
  if (style.indent) css.paddingLeft = `${4 + style.indent * 8}px`;
  if (style.border) {
    const { top, right, bottom, left } = style.border;
    const t = borderCss(top);
    const r = borderCss(right);
    const b = borderCss(bottom);
    const l = borderCss(left);
    if (t) css.borderTop = t;
    if (r) css.borderRight = r;
    if (b) css.borderBottom = b;
    if (l) css.borderLeft = l;
  }
  return css;
}
