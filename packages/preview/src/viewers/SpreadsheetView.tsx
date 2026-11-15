// 表格预览 —— 把 SheetModel 铺成一张「看起来就是 Excel」的网格。
//
// 三条不可退让的保真线（都来自源文件，不是美化）：
//   1. 行 1 也是数据。旧实现把第一行当表头吃掉，等于每张表少一行数据。
//   2. 每个格子的底色 / 字色 / 字重 / 边框 / 对齐 / 数字格式来自 styles.xml。
//      旧实现只读值、格式全丢——这就是"格式没有还原"的直接原因。
//   3. 合并区、列宽、行高、冻结窗格、隐藏行列按文件原样呈现。
//
// 只对**行**做窗口化：真正会炸的是行数（两万行的表），列数上限已经在
// workbook.ts 里卡到 512，45 个可见行 × 512 列仍在 DOM 可承受范围内。
// 行高来自源文件、常不均匀，所以窗口用「行高前缀和 + 二分」求，
// 前后各补一个撑高占位行——写死行高会在自定义行高的表上错位。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { ChevronLeft, ChevronRight, Download, Search, X } from "lucide-react";
import type { CellModel, ParsedWorkbook, SheetModel } from "../lib/workbook";
import { contrastInk, DEFAULT_ROW_H, styleToCss } from "../lib/workbook";
import { colName } from "../lib/localAssets";

const ROW_HEADER_W = 46;
const HEADER_H = 26;
/** Excel 默认列宽 8.43 字符 ≈ 64px。 */
const DEFAULT_COL_W = 64;
const MIN_COL_W = 36;
const OVERSCAN_ROWS = 8;
/** 行数超过这个量才开窗口化，小表直接全量渲染，避免无谓的复杂度。 */
const VIRTUAL_ROW_THRESHOLD = 400;

interface RowWindow {
  start: number;
  end: number;
  topPad: number;
  bottomPad: number;
}

/**
 * 前缀和 + 二分：把滚动位置换算成可见行区间与上下补白高度。
 * offsets[i] = 第 i 个可见行之前所有可见行的总高，末项是总高。
 */
function windowFromOffsets(
  offsets: number[],
  scrollTop: number,
  viewport: number,
  overscan: number,
): RowWindow {
  const total = offsets[offsets.length - 1] ?? 0;
  let lo = 0;
  let hi = offsets.length - 1;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if ((offsets[mid] ?? 0) <= scrollTop) lo = mid;
    else hi = mid - 1;
  }
  const start = Math.max(0, lo - overscan);
  let end = lo;
  while (end < offsets.length - 1 && (offsets[end] ?? 0) < scrollTop + viewport) end++;
  end = Math.min(offsets.length, end + 1 + overscan);
  return {
    start,
    end,
    topPad: offsets[start] ?? 0,
    bottomPad: total - (offsets[end] ?? total),
  };
}

/* ── 搜索命中 ───────────────────────────────────────────────────── */

/** 逐格比对返回命中集合。空查询返回 null，调用方据此跳过高亮分支。 */
function findMatches(sheet: SheetModel, query: string): Set<string> | null {
  const q = query.trim().toLowerCase();
  if (!q) return null;
  const hits = new Set<string>();
  for (let r = 0; r < sheet.rows.length; r++) {
    const row = sheet.rows[r];
    if (!row) continue;
    for (let c = 0; c < row.length; c++) {
      const text = row[c]?.text;
      if (text && text.toLowerCase().includes(q)) hits.add(`${r}:${c}`);
    }
  }
  return hits;
}

/* ══════════════════════════════════════════════════════════════ */

export function SpreadsheetView({
  model,
  fileName,
  zoom,
}: {
  model: ParsedWorkbook;
  fileName: string;
  zoom: number;
}) {
  const sheets = model.sheets;
  const [active, setActive] = useState(0);
  const [query, setQuery] = useState("");

  useEffect(() => {
    setActive(0);
    setQuery("");
  }, [fileName]);

  const sheet = sheets[Math.min(active, sheets.length - 1)];

  const exportCsv = useCallback(async () => {
    if (!sheet) return;
    const { sheetToCsv } = await import("../lib/workbook");
    const csv = await sheetToCsv(sheet);
    const url = URL.createObjectURL(new Blob([csv], { type: "text/csv;charset=utf-8" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `${fileName.replace(/\.[^.]+$/, "")}.csv`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 200);
  }, [sheet, fileName]);

  const matchCount = useMemo(() => findMatches(sheet, query)?.size ?? 0, [sheet, query]);

  if (!sheet) {
    return <div className="sheet-empty">没有可显示的工作表</div>;
  }

  return (
    <div className="sheet-root">
      <div className="sheet-bar">
        {sheets.length > 1 && (
          <div className="sheet-switch">
            <button
              type="button"
              className="sheet-step"
              onClick={() => setActive((i) => (i - 1 + sheets.length) % sheets.length)}
              title="上一个工作表"
              aria-label="上一个工作表"
            >
              <ChevronLeft className="icon-sm" />
            </button>
            <span className="sheet-name" title={sheet.name}>
              {sheet.name}
            </span>
            <button
              type="button"
              className="sheet-step"
              onClick={() => setActive((i) => (i + 1) % sheets.length)}
              title="下一个工作表"
              aria-label="下一个工作表"
            >
              <ChevronRight className="icon-sm" />
            </button>
          </div>
        )}

        <div className="sheet-search">
          <Search className="icon-sm sheet-search-icon" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="在表格内搜索…"
            aria-label="在表格内搜索"
          />
          {query && (
            <>
              <span className="sheet-hits">{matchCount}</span>
              <button
                type="button"
                onClick={() => setQuery("")}
                title="清空搜索"
                aria-label="清空搜索"
              >
                <X className="icon-sm" />
              </button>
            </>
          )}
        </div>

        <div className="sheet-bar-spacer" />

        {model.fidelity === "plain" && (
          <span
            className="sheet-badge"
            title=".xls 是 BIFF 二进制、CSV 是纯文本，两者的样式表无法读取，因此只还原内容。"
          >
            仅内容
          </span>
        )}

        <button
          type="button"
          className="sheet-action"
          onClick={() => void exportCsv()}
          title="导出当前工作表为 CSV"
        >
          <Download className="icon-sm" />
          导出 CSV
        </button>
      </div>

      <SheetGrid
        key={sheet.name}
        sheet={sheet}
        zoom={zoom}
        query={query}
        emptyHint={
          sheet.rowCount === 0 ? "工作表为空" : query ? "没有匹配的数据" : undefined
        }
      />
    </div>
  );
}

/* ══════════════════════════════════════════════════════════════
   网格本体
   ══════════════════════════════════════════════════════════════ */

function SheetGrid({
  sheet,
  zoom,
  query,
  emptyHint,
}: {
  sheet: SheetModel;
  zoom: number;
  query: string;
  emptyHint?: string;
}) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewport, setViewport] = useState(600);

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    setViewport(el.clientHeight);
    const ro = new ResizeObserver(() => setViewport(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  // 隐藏行列不占位：显示序列与原文件索引分开存，用 colIndex 回查原文件。
  const cols = useMemo(
    () => Array.from({ length: sheet.colCount }, (_, i) => i).filter((i) => !sheet.hiddenCols.has(i)),
    [sheet.colCount, sheet.hiddenCols],
  );
  const rowIndexes = useMemo(
    () => Array.from({ length: sheet.rowCount }, (_, i) => i).filter((i) => !sheet.hiddenRows.has(i)),
    [sheet.rowCount, sheet.hiddenRows],
  );

  const widths = useMemo(
    () => cols.map((i) => Math.max(MIN_COL_W, sheet.colWidths[i] ?? DEFAULT_COL_W)),
    [cols, sheet.colWidths],
  );

  const rowOffsets = useMemo(() => {
    const acc = [0];
    for (const r of rowIndexes) {
      acc.push(acc[acc.length - 1] + (sheet.rowHeights[r] ?? DEFAULT_ROW_H));
    }
    return acc;
  }, [rowIndexes, sheet.rowHeights]);

  const win = useMemo(() => {
    if (rowIndexes.length <= VIRTUAL_ROW_THRESHOLD) {
      return { start: 0, end: rowIndexes.length, topPad: 0, bottomPad: 0 };
    }
    return windowFromOffsets(rowOffsets, scrollTop, viewport, OVERSCAN_ROWS);
  }, [rowIndexes.length, rowOffsets, scrollTop, viewport]);

  const hits = useMemo(() => findMatches(sheet, query), [sheet, query]);

  // 合并区主格若在窗口之上而覆盖区伸进窗口，主格必须一起渲染，否则内容丢失。
  const rowStart = useMemo(() => {
    let start = win.start;
    for (const m of sheet.merges) {
      if (m.row < start && m.row + m.rowSpan - 1 >= start) start = m.row;
    }
    return start;
  }, [win.start, sheet.merges]);

  const mergeSpan = useMemo(() => {
    const map = new Map<string, { rs: number; cs: number }>();
    for (const m of sheet.merges) {
      if (m.rowSpan > 1 || m.colSpan > 1) map.set(`${m.row}:${m.col}`, { rs: m.rowSpan, cs: m.colSpan });
    }
    return map;
  }, [sheet.merges]);

  // 冻结窗格：源文件冻几行几列，表头就钉在那个位置。
  const frozenRows = Math.min(sheet.frozen?.y ?? 0, rowIndexes.length);
  const frozenCols = Math.min(sheet.frozen?.x ?? 0, cols.length);
  const frozenRowTops = useMemo(() => {
    const tops: number[] = [];
    for (let i = 0; i < frozenRows; i++) {
      tops.push(rowOffsets[i + 1] - rowOffsets[i]);
    }
    let acc = 0;
    return tops.map((h) => {
      const top = acc;
      acc += h;
      return top;
    });
  }, [frozenRows, rowOffsets]);

  const span = cols.length + 1;
  const visibleRows = rowIndexes.slice(rowStart, win.end);

  return (
    <div className="sheet-scroll" ref={scrollRef} onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}>
      <table className="sheet-table" style={{ zoom: zoom / 100 }}>
        <colgroup>
          <col style={{ width: ROW_HEADER_W }} />
          {widths.map((w, i) => (
            <col key={cols[i]} style={{ width: w }} />
          ))}
        </colgroup>
        <thead>
          <tr>
            <th className="sheet-corner" scope="col">
              <span className="sheet-corner-mark" aria-hidden="true" />
            </th>
            {cols.map((c, n) => (
              <th
                key={c}
                scope="col"
                className="sheet-col-head"
                style={n < frozenCols ? { left: ROW_HEADER_W + sumBefore(widths, n) } : undefined}
              >
                {colName(c)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {win.topPad > 0 && (
            <tr style={{ height: win.topPad }} aria-hidden="true">
              <td colSpan={span} className="sheet-pad" />
            </tr>
          )}
          {visibleRows.map((r, n) => (
            <tr key={r} style={{ height: sheet.rowHeights[r] ?? DEFAULT_ROW_H }}>
              <th
                scope="row"
                className="sheet-row-head"
                style={n < frozenRows ? { top: HEADER_H + (frozenRowTops[n] ?? 0) } : undefined}
              >
                {r + 1}
              </th>
              {cols.map((c, cn) => (
                <SheetCell
                  key={c}
                  cell={sheet.rows[r]?.[c] ?? null}
                  merge={mergeSpan.get(`${r}:${c}`)}
                  hit={hits ? hits.has(`${r}:${c}`) : false}
                  frozenCol={cn < frozenCols}
                  frozenLeft={ROW_HEADER_W + sumBefore(widths, cn)}
                  rowHeight={sheet.rowHeights[r] ?? DEFAULT_ROW_H}
                  spill={canSpill(sheet, r, cols, cn)}
                />
              ))}
            </tr>
          ))}
          {win.bottomPad > 0 && (
            <tr style={{ height: win.bottomPad }} aria-hidden="true">
              <td colSpan={span} className="sheet-pad" />
            </tr>
          )}
          {rowIndexes.length === 0 && (
            <tr>
              <td colSpan={span} className="sheet-empty-cell">
                {emptyHint ?? "工作表为空"}
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

function sumBefore(values: number[], n: number): number {
  let acc = 0;
  for (let i = 0; i < n; i++) acc += values[i] ?? 0;
  return acc;
}

/**
 * Excel 的文本溢出规则：右侧相邻格为空且无底色时，文字直接铺过去。
 * 相邻格有内容或有填充时不溢出（填充会盖住铺过去的文字，与 Excel 一致），
 * 退回省略号 + 悬停看全文。
 */
function canSpill(sheet: SheetModel, row: number, cols: number[], colIndex: number): boolean {
  const nextCol = cols[colIndex + 1];
  if (nextCol === undefined) return false;
  const neighbor = sheet.rows[row]?.[nextCol];
  if (!neighbor) return true;
  return !neighbor.text && neighbor.style?.bg === undefined;
}

function SheetCell({
  cell,
  merge,
  hit,
  frozenCol,
  frozenLeft,
  rowHeight,
  spill,
}: {
  cell: CellModel | null;
  merge?: { rs: number; cs: number };
  hit: boolean;
  frozenCol: boolean;
  frozenLeft: number;
  rowHeight: number;
  spill: boolean;
}) {
  const style = cell?.style;
  const css: CSSProperties = {
    // 没有显式颜色的格子：按底色亮度选墨色（白底黑字 / 深底白字）。
    // 沿用界面主题的字色会出事：深色界面下浅色字压在浅色底上 = 看不见。
    ...(style?.color ? {} : { color: contrastInk(style?.bg) }),
    // 冻结列会横向滚动，没有底色的格子会让下层单元格透出来。源文件给了
    // 填充色就用它，没给就补纸面底色。
    ...(frozenCol && !style?.bg ? { backgroundColor: "var(--sheet-bg, #ffffff)" } : null),
    // Excel 的默认垂直对齐是底对齐，不是浏览器的中间对齐。
    ...(style?.valign ? {} : { verticalAlign: "bottom" as const }),
    ...styleToCss(style),
    ...(style?.rotation
      ? ({ writingMode: "vertical-rl", textOrientation: "mixed" } as CSSProperties)
      : null),
    ...(merge && merge.rs > 1 ? { height: rowHeight * merge.rs } : null),
    ...(frozenCol ? { position: "sticky", left: frozenLeft, zIndex: 2 } : null),
  };
  // spill 的格子让文字溢出到右边邻居；否则裁切并省略号。
  if (style?.wrap || !spill) {
    css.overflow = "hidden";
    css.textOverflow = "ellipsis";
  } else {
    // 文字要真的铺到邻居身上：overflow 必须显式回到 visible，位置也要
    // 提一层（否则后一个格子的底色会盖住铺过来的字 —— Excel 里是盖不住的）。
    css.overflow = "visible";
    if (!frozenCol) {
      css.position = "relative";
      css.zIndex = 1;
    }
  }

  const title = cell?.formula
    ? `= ${cell.formula}${cell.text ? `\n${cell.text}` : ""}`
    : cell?.text || undefined;

  const content = cell?.href ? (
    <a
      className="sheet-link"
      href={cell.href}
      target="_blank"
      rel="noopener noreferrer"
      title={cell.href}
    >
      {cell.text}
    </a>
  ) : (
    cell?.text
  );

  return (
    <td
      className={hit ? "sheet-cell is-hit" : "sheet-cell"}
      style={css}
      colSpan={merge?.cs}
      rowSpan={merge?.rs}
      title={title}
    >
      {content}
    </td>
  );
}
