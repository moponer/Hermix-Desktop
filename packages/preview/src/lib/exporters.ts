//! 文档画布导出旁路的转换器：画布 HTML → xlsx / docx。
//!
//! 分层：`htmlToXlsxModel` 是**纯 DOM 解析**（零 ExcelJS 依赖，可脱离
//! 渲染引擎单测）；`modelToXlsxBytes` / `htmlToDocxBytes` 是渲染层，
//! 引擎按需动态加载（不进主包）。
//!
//! 质量红线（officecli 教训）：转换器输出必须结构正确——合并单元格走
//! 真实的网格占位算法（colspan/rowspan 展开成网格坐标），不是"每格一格"
//! 的近似；表头检测按 `<th>` 语义而不是猜第一行。

import type { CanvasPageGeometry } from "./docCanvasStore";

// ── xlsx：模型层 ────────────────────────────────────────────────

export interface XlsxMerge {
  /** 0 基（含端点）；渲染层 +1 转 ExcelJS 的 1 基。 */
  r1: number;
  c1: number;
  r2: number;
  c2: number;
}

export interface XlsxSheetModel {
  name: string;
  /** 规整后的网格：null = 被合并覆盖的空位。 */
  rows: (string | number | null)[][];
  merges: XlsxMerge[];
  /** 第一行含 <th> → 渲染层加粗表头。 */
  header: boolean;
}

export interface XlsxModel {
  sheets: XlsxSheetModel[];
}

/** "1,234.56" → 1234.56；其余原样字符串。百分比/货币保留字符串。 */
function cellValue(raw: string): string | number {
  const text = raw.replace(/\s+/g, " ").trim();
  if (/^-?[\d,]+(\.\d+)?$/.test(text) && /\d/.test(text)) {
    const n = Number(text.replace(/,/g, ""));
    if (Number.isFinite(n)) return n;
  }
  return text;
}

/**
 * 单表 → 网格模型。colspan/rowspan 展开成真实网格坐标（合并区内的
 * 覆盖位记 null），这是 ExcelJS mergeCells 的坐标来源。
 */
function sheetFromTable(table: HTMLTableElement, index: number): XlsxSheetModel {
  const rows: (string | number | null)[][] = [];
  const occupied: boolean[][] = [];
  const merges: XlsxMerge[] = [];
  let header = false;

  // HTMLTableElement.rows 只含本表（含 thead/tbody/tfoot）的行，
  // 嵌套表的行不会混入。jsdom 与浏览器行为一致。
  Array.from(table.rows).forEach((tr, r) => {
    rows[r] = rows[r] ?? [];
    occupied[r] = occupied[r] ?? [];
    let c = 0;
    for (const cell of Array.from(tr.cells)) {
      while (occupied[r][c]) c++;
      const cs = Math.max(1, Math.min(cell.colSpan || 1, 64));
      const rs = Math.max(1, Math.min(cell.rowSpan || 1, 64));
      rows[r][c] = cellValue(cell.textContent ?? "");
      for (let dr = 0; dr < rs; dr++) {
        for (let dc = 0; dc < cs; dc++) {
          if (dr === 0 && dc === 0) continue;
          const rr = r + dr;
          const cc = c + dc;
          occupied[rr] = occupied[rr] ?? [];
          occupied[rr][cc] = true;
          rows[rr] = rows[rr] ?? [];
          rows[rr][cc] = null;
        }
      }
      if (cs > 1 || rs > 1) merges.push({ r1: r, c1: c, r2: r + rs - 1, c2: c + cs - 1 });
      if (cell.tagName === "TH" && r === 0) header = true;
      c += cs;
    }
  });

  // 行长规整：补齐到最宽行（ExcelJS addRows 要求等长才对齐列）。
  const width = rows.reduce((w, row) => Math.max(w, row?.length ?? 0), 0);
  for (const row of rows) {
    for (let c = 0; c < width; c++) row[c] = row[c] ?? null;
  }

  // 表名：<caption> 优先（标准语义），否则 SheetN。非法字符剔除。
  const caption = table.caption?.textContent?.replace(/\s+/g, " ").trim();
  const safe = (caption ?? `Sheet${index + 1}`)
    .replace(/[\\/*?:[\]]/g, "")
    .slice(0, 31) || `Sheet${index + 1}`;

  return { name: safe, rows, merges, header };
}

/**
 * 画布 HTML → xlsx 模型。没有 <table> 时返回 null（调用方提示"文档中
 * 没有表格"，而不是导出一个空工作簿）。
 */
export function htmlToXlsxModel(html: string): XlsxModel | null {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const tables = Array.from(doc.querySelectorAll("table")) as HTMLTableElement[];
  if (tables.length === 0) return null;
  return { sheets: tables.map((t, i) => sheetFromTable(t, i)) };
}

// ── xlsx：渲染层（ExcelJS 按需加载）────────────────────────────

/** 模型 → .xlsx 字节。引擎动态加载，不进主包。 */
export async function modelToXlsxBytes(model: XlsxModel): Promise<Uint8Array> {
  const { Workbook } = await import("exceljs");
  const wb = new Workbook();
  for (const sheet of model.sheets) {
    const ws = wb.addWorksheet(sheet.name);
    ws.addRows(sheet.rows);
    for (const m of sheet.merges) {
      ws.mergeCells(m.r1 + 1, m.c1 + 1, m.r2 + 1, m.c2 + 1);
    }
    if (sheet.header) {
      const headerRow = ws.getRow(1);
      headerRow.font = { bold: true };
      headerRow.alignment = { vertical: "middle" };
    }
    // 列宽：按内容最大字符数估算（CJK 按 2 计），钳制 10..60。
    const widths = new Array<number>(sheet.rows[0]?.length ?? 0).fill(10);
    for (const row of sheet.rows.slice(0, 200)) {
      row.forEach((v, c) => {
        const len = [...String(v ?? "")].reduce(
          (n, ch) => n + (ch.charCodeAt(0) > 0xff ? 2 : 1),
          0,
        );
        widths[c] = Math.max(widths[c] ?? 10, len + 2);
      });
    }
    widths.forEach((w, i) => {
      ws.getColumn(i + 1).width = Math.min(60, Math.max(10, w));
    });
  }
  const buf = await wb.xlsx.writeBuffer();
  return new Uint8Array(buf as ArrayBuffer);
}

/** 便捷组合：HTML → xlsx 字节；无表格时返回 null。 */
export async function htmlToXlsxBytes(html: string): Promise<Uint8Array | null> {
  const model = htmlToXlsxModel(html);
  if (!model) return null;
  return modelToXlsxBytes(model);
}

// ── docx：IR 层（纯 DOM 解析，可单测）───────────────────────────

export interface DocxSpan {
  text: string;
  bold?: boolean;
  italic?: boolean;
}

export interface DocxTableCell {
  spans: DocxSpan[];
  /** <th> 单元格（按语义，不是猜首行）。 */
  header: boolean;
  colSpan: number;
  rowSpan: number;
}

export type DocxBlock =
  | { kind: "heading"; level: number; spans: DocxSpan[] }
  | { kind: "para"; spans: DocxSpan[] }
  | { kind: "list"; ordered: boolean; items: { level: number; spans: DocxSpan[] }[] }
  | { kind: "quote"; spans: DocxSpan[] }
  | { kind: "table"; rows: DocxTableCell[][]; header: boolean }
  | { kind: "pagebreak" };

/** 行内 span 收集：继承 bold/italic（strong/em 嵌套语义），br → 换行符。
 *  只在 true 时携带键——显式 false 会污染下游 JSON 对比与 docx 属性。 */
function collectSpans(
  node: Node,
  inherited: { bold?: boolean; italic?: boolean },
  spans: DocxSpan[],
): void {
  for (const child of Array.from(node.childNodes)) {
    if (child.nodeType === 3) {
      const text = child.textContent ?? "";
      if (text) spans.push({ text, ...inherited });
      continue;
    }
    if (child.nodeType !== 1) continue;
    const el = child as Element;
    const tag = el.tagName.toLowerCase();
    if (tag === "style" || tag === "script") continue;
    if (tag === "br") {
      spans.push({ text: "\n", ...inherited });
      continue;
    }
    const next: { bold?: boolean; italic?: boolean } = { ...inherited };
    if (tag === "strong" || tag === "b") next.bold = true;
    if (tag === "em" || tag === "i") next.italic = true;
    collectSpans(el, next, spans);
  }
}

/** docx 表格收集：保留原生 colSpan/rowSpan（docx TableCell 原生支持，
 *  不需要像 xlsx 那样展开成网格坐标）。 */
function collectTable(table: HTMLTableElement): DocxBlock {
  const rows: DocxTableCell[][] = [];
  let header = false;
  Array.from(table.rows).forEach((tr) => {
    const cells = Array.from(tr.cells).map((cell) => {
      if (cell.tagName === "TH" && rows.length === 0) header = true;
      const spans: DocxSpan[] = [];
      collectSpans(cell, { bold: cell.tagName === "TH" }, spans);
      return {
        spans,
        header: cell.tagName === "TH",
        colSpan: Math.max(1, Math.min(cell.colSpan || 1, 64)),
        rowSpan: Math.max(1, Math.min(cell.rowSpan || 1, 64)),
      };
    });
    if (cells.length > 0) rows.push(cells);
  });
  return { kind: "table", rows, header };
}

/** li 项收集：嵌套列表 → 更深 level（缩进），一层 li 可产多个项。 */
function collectListItems(
  list: Element,
  level: number,
  out: { level: number; spans: DocxSpan[] }[],
): void {
  for (const li of Array.from(list.children)) {
    if (li.tagName.toLowerCase() !== "li") continue;
    // 直接文本先出项；子列表递归为更深 level。
    const own = li.cloneNode(true) as Element;
    own.querySelectorAll("ul,ol").forEach((n) => n.remove());
    const spans: DocxSpan[] = [];
    collectSpans(own, {}, spans);
    if (spans.some((s) => s.text.trim())) out.push({ level, spans });
    for (const sub of Array.from(li.children)) {
      const t = sub.tagName.toLowerCase();
      if (t === "ul" || t === "ol") collectListItems(sub, level + 1, out);
    }
  }
}

/** 块遍历：canvas 块（section/slide）递归其子块；slide 之间出分页。 */
function walkBlocks(
  container: Element,
  geometry: CanvasPageGeometry,
  out: DocxBlock[],
  slideDepth = 0,
): void {
  for (const el of Array.from(container.children)) {
    const tag = el.tagName.toLowerCase();
    if (tag === "style" || tag === "script") continue;
    if (tag === "slide" || tag === "section") {
      if (tag === "slide" && slideDepth === 0 && out.length > 0) {
        out.push({ kind: "pagebreak" });
      }
      walkBlocks(el, geometry, out, tag === "slide" ? slideDepth + 1 : slideDepth);
      continue;
    }
    switch (tag) {
      case "h1": case "h2": case "h3": case "h4": case "h5": case "h6": {
        const spans: DocxSpan[] = [];
        collectSpans(el, {}, spans);
        out.push({ kind: "heading", level: Number(tag[1]), spans });
        break;
      }
      case "p": {
        const spans: DocxSpan[] = [];
        collectSpans(el, {}, spans);
        if (spans.some((s) => s.text.trim())) out.push({ kind: "para", spans });
        break;
      }
      case "ul": case "ol": {
        const items: { level: number; spans: DocxSpan[] }[] = [];
        collectListItems(el, 0, items);
        if (items.length > 0) out.push({ kind: "list", ordered: tag === "ol", items });
        break;
      }
      case "blockquote": {
        const spans: DocxSpan[] = [];
        collectSpans(el, {}, spans);
        if (spans.some((s) => s.text.trim())) out.push({ kind: "quote", spans });
        break;
      }
      case "table":
        out.push(collectTable(el as HTMLTableElement));
        break;
      default: {
        // 未知容器（div 等）递归；叶子元素按段落兜底。
        if (el.children.length > 0) walkBlocks(el, geometry, out, slideDepth);
        else {
          const spans: DocxSpan[] = [];
          collectSpans(el, {}, spans);
          if (spans.some((s) => s.text.trim())) out.push({ kind: "para", spans });
        }
      }
    }
  }
}

/** 画布 HTML → docx 块 IR。slide 几何在 slide 块之间插分页；a4 自然流式。 */
export function htmlToDocxBlocks(
  html: string,
  geometry: CanvasPageGeometry,
): DocxBlock[] {
  const doc = new DOMParser().parseFromString(html, "text/html");
  const out: DocxBlock[] = [];
  walkBlocks(doc.body, geometry, out);
  return out;
}

// ── docx：渲染层（docx 引擎按需加载）────────────────────────────

/** IR → .docx 字节。样式"干净不花哨"（拍板口径）：内置 Heading 层级、
 *  无序列表用原生 bullet、有序列表用文本编号前缀、表格原生 span。 */
export async function htmlToDocxBytes(
  html: string,
  geometry: CanvasPageGeometry,
): Promise<Uint8Array> {
  const blocks = htmlToDocxBlocks(html, geometry);
  const d = await import("docx");
  // 类型经顶层 type import（docx 的类型在渲染层函数签名处也要用），
  // 运行时仍走动态 import——引擎不进主包。
  const children: Array<InstanceType<typeof d.Paragraph> | InstanceType<typeof d.Table>> = [];
  let orderedCounter = 0;

  const runsOf = (spans: DocxSpan[], extra?: { bold?: boolean; italics?: boolean }) =>
    spans.map(
      (s) =>
        new d.TextRun({
          text: s.text,
          bold: s.bold || extra?.bold,
          italics: s.italic || extra?.italics,
        }),
    );

  for (const b of blocks) {
    switch (b.kind) {
      case "pagebreak":
        children.push(new d.Paragraph({ children: [new d.PageBreak()] }));
        break;
      case "heading": {
        const levels = [
          d.HeadingLevel.HEADING_1, d.HeadingLevel.HEADING_2, d.HeadingLevel.HEADING_3,
          d.HeadingLevel.HEADING_4, d.HeadingLevel.HEADING_5, d.HeadingLevel.HEADING_6,
        ];
        children.push(
          new d.Paragraph({ heading: levels[b.level - 1], children: runsOf(b.spans) }),
        );
        break;
      }
      case "para":
        children.push(new d.Paragraph({ children: runsOf(b.spans) }));
        break;
      case "quote":
        children.push(
          new d.Paragraph({
            children: runsOf(b.spans, { italics: true }),
            indent: { left: 720 },
          }),
        );
        break;
      case "list":
        for (const item of b.items) {
          if (b.ordered) {
            orderedCounter += 1;
            children.push(
              new d.Paragraph({
                children: [
                  new d.TextRun({ text: `${orderedCounter}. `, bold: false }),
                  ...runsOf(item.spans),
                ],
                indent: { left: 360 + item.level * 360 },
              }),
            );
          } else {
            children.push(
              new d.Paragraph({
                children: runsOf(item.spans),
                bullet: { level: Math.min(item.level, 8) },
              }),
            );
          }
        }
        break;
      case "table": {
        const rows = b.rows.map(
          (cells) =>
            new d.TableRow({
              children: cells.map(
                (cell) =>
                  new d.TableCell({
                    columnSpan: cell.colSpan > 1 ? cell.colSpan : undefined,
                    rowSpan: cell.rowSpan > 1 ? cell.rowSpan : undefined,
                    children: [
                      new d.Paragraph({ children: runsOf(cell.spans) }),
                    ],
                  }),
              ),
            }),
        );
        children.push(
          new d.Table({ rows, width: { size: 100, type: d.WidthType.PERCENTAGE } }),
        );
        break;
      }
    }
  }

  const doc = new d.Document({ sections: [{ children }] });
  const buf = await d.Packer.toBuffer(doc);
  return new Uint8Array(buf as unknown as ArrayBuffer);
}
