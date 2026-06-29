/**
 * SpreadsheetView — renders a parsed sheet/csv as a real `<table>`
 * with frozen header, in-table search, row virtualization for big
 * files, sheet switcher, and CSV export.
 *
 * Why this exists (2026-06-22): the previous "office preview" was a
 * raw HTML blob from the sidecar `officecli` binary, which produced
 * a degenerate "Plain text" view for any xlsx on the user's machine
 * (officecli not installed, fallback path dumps the binary as text).
 * This view is rendered entirely from in-memory rows parsed by the
 * frontend (see `lib/fileParser.ts`), so:
 *   - no backend dependency (no LibreOffice)
 *   - real <table> cells, not a pre-baked HTML blob
 *   - features we can layer on: header freeze, in-table search,
 *     column resize, row virtualization, sheet switcher, CSV export
 *
 * Performance: for files > 1 000 rows we render only the visible
 * window (row windowing) using `transform: translateY` on a single
 * virtual layer. The header row stays pinned to the top via a
 * sticky `<thead>`. This keeps the first-paint cheap even for a
 * 50 MiB xlsx that the user has scrolled deep into.
 */

import { useEffect, useMemo, useRef, useState } from "react";
import { Download, Search, ChevronLeft, ChevronRight, X } from "lucide-react";
import type { PreviewModel } from "../lib/fileParser";
import { sheetToCsv } from "../lib/fileParser";

const ROW_HEIGHT = 28; // px — must match the <tr> CSS height
const VIEWPORT_OVERSCAN = 12; // extra rows above/below the visible window

type SheetRows = { name: string; rows: string[][]; rowCount: number; colCount: number };

interface SpreadsheetViewProps {
  /** The pre-parsed preview model. The parent decides which file
   * to load and hands us the parsed result. We don't talk to
   * disk here. */
  model: PreviewModel;
  fileName: string;
}

export function SpreadsheetView({ model, fileName }: SpreadsheetViewProps) {
  // Unwrap the model into the concrete shape this view expects
  const sheets: SheetRows[] = useMemo(() => {
    if (model.kind === "spreadsheet") return model.sheets;
    if (model.kind === "csv") {
      return [
        { name: fileName, rows: model.rows, rowCount: model.rowCount, colCount: model.colCount },
      ];
    }
    return [];
  }, [model, fileName]);

  const [activeSheet, setActiveSheet] = useState(0);
  const [query, setQuery] = useState("");

  // Reset to first sheet when the file changes
  useEffect(() => {
    setActiveSheet(0);
    setQuery("");
  }, [fileName]);

  if (sheets.length === 0) {
    return (
      <div className="text-[12px] text-text-muted p-4 text-center">
        没有可显示的工作表
      </div>
    );
  }

  const current = sheets[activeSheet];

  // Filter rows by query. We treat row 0 as headers (case-insensitive
  // substring match) — if no headers exist, every row is data. The
  // search matches any cell in the row, NOT just one column, so the
  // user can type a known order ID / employee name etc.
  const filtered = useMemo(() => {
    if (!query) return current.rows;
    const q = query.toLowerCase();
    return current.rows.filter((row) =>
      row.some((cell) => (cell ?? "").toString().toLowerCase().includes(q)),
    );
  }, [current.rows, query]);

  // Identify the header row — convention: the first row, unless it
  // is empty. We pin it via sticky positioning so it stays visible
  // during scroll.
  const headerRow = current.rows[0] ?? [];
  // When `query` is active, `filtered` already includes the header
  // (we didn't strip it before the filter), so we re-derive data rows
  // from the filtered output. To keep things simple, recompute:
  const filteredHeader = query ? (filtered[0] ?? headerRow) : headerRow;
  const filteredData = query ? filtered.slice(1) : current.rows.slice(1);

  const handleExport = () => {
    const csv = sheetToCsv({ name: current.name, rows: current.rows });
    const blob = new Blob([csv], { type: "text/csv;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${current.name.replace(/\.(xlsx?|xlsm|csv|tsv)$/i, "")}.csv`;
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 200);
  };

  return (
    <div className="flex flex-col h-full min-h-0">
      {/* Sheet tabs + search + export */}
      <div
        className="flex items-center gap-2 px-2 py-1.5 border-b shrink-0"
        style={{ borderColor: "var(--color-border-subtle)" }}
      >
        {/* Sheet switcher — chevron + label + chevron, classic Excel feel */}
        {sheets.length > 1 && (
          <div className="flex items-center gap-0.5">
            <button
              type="button"
              onClick={() => setActiveSheet((i) => (i - 1 + sheets.length) % sheets.length)}
              disabled={sheets.length <= 1}
              className="p-1 rounded hover:bg-bg-hover disabled:opacity-30 text-text-muted"
              title="上一个工作表"
            >
              <ChevronLeft size={11} />
            </button>
            <span className="text-[11.5px] font-medium px-1.5 min-w-[60px] text-center truncate text-text-primary">
              {current.name}
            </span>
            <button
              type="button"
              onClick={() => setActiveSheet((i) => (i + 1) % sheets.length)}
              disabled={sheets.length <= 1}
              className="p-1 rounded hover:bg-bg-hover disabled:opacity-30 text-text-muted"
              title="下一个工作表"
            >
              <ChevronRight size={11} />
            </button>
          </div>
        )}

        {/* Search input */}
        <div className="flex items-center gap-1.5 flex-1 min-w-0 max-w-[280px] px-2 py-1 rounded bg-bg-tertiary">
          <Search size={11} className="text-text-muted shrink-0" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="在表格内搜索…"
            className="flex-1 bg-transparent text-[11.5px] outline-none min-w-0 placeholder:text-text-faint"
          />
          {query && (
            <button
              type="button"
              onClick={() => setQuery("")}
              className="text-text-faint hover:text-text-muted"
              title="清空搜索"
            >
              <X size={10} />
            </button>
          )}
        </div>

        <div className="flex-1" />

        {/* Export CSV */}
        <button
          type="button"
          onClick={handleExport}
          className="flex items-center gap-1 text-[11px] px-2 py-1 rounded hover:bg-bg-hover text-text-muted hover:text-text-primary"
          title="导出当前工作表为 CSV"
        >
          <Download size={11} />
          导出 CSV
        </button>
      </div>

      {/* Table area */}
      <SheetTable header={filteredHeader} data={filteredData} />
    </div>
  );
}

interface SheetTableProps {
  header: string[];
  data: string[][];
}

/** The actual scrollable table. Renders the header sticky and
 * virtualises the body when there are more than 1 000 rows. */
function SheetTable({ header, data }: SheetTableProps) {
  const scrollRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(600);

  // Track scroll position + viewport height for the row windowing
  // math. We measure with a ResizeObserver so the window adjusts
  // when the user toggles fullscreen or resizes the sidebar.
  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => {
      setViewportHeight(el.clientHeight);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const onScroll = (e: React.UIEvent<HTMLDivElement>) => {
    setScrollTop(e.currentTarget.scrollTop);
  };

  // Decide whether virtualisation is worth the trouble. For small
  // sheets we render everything — simpler code, no off-by-one bugs.
  const useVirtual = data.length > 1000;
  const totalHeight = data.length * ROW_HEIGHT;
  const startIdx = useVirtual
    ? Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - VIEWPORT_OVERSCAN)
    : 0;
  const visibleCount = useVirtual
    ? Math.ceil(viewportHeight / ROW_HEIGHT) + VIEWPORT_OVERSCAN * 2
    : data.length;
  const endIdx = Math.min(data.length, startIdx + visibleCount);
  const window = data.slice(startIdx, endIdx);

  const colCount = Math.max(header.length, data.reduce((m, r) => Math.max(m, r.length), 0));

  return (
    <div
      ref={scrollRef}
      onScroll={onScroll}
      className="flex-1 min-h-0 overflow-auto"
      style={{ background: "var(--color-bg-primary)" }}
    >
      <table
        className="text-[11.5px] font-sans"
        style={{
          borderCollapse: "separate",
          borderSpacing: 0,
          width: "max-content",
          minWidth: "100%",
        }}
      >
        <thead>
          <tr>
            {/* Row number column — Excel-style 1-based row numbers */}
            <th
              className="sticky top-0 z-20 text-center font-semibold"
              style={{
                width: 38,
                minWidth: 38,
                padding: "0 6px",
                height: ROW_HEIGHT,
                background: "var(--color-bg-tertiary)",
                color: "var(--color-text-muted)",
                borderBottom: "1px solid var(--color-border-subtle)",
                borderRight: "1px solid var(--color-border-subtle)",
                position: "sticky",
                left: 0,
                top: 0,
              }}
            >
              #
            </th>
            {Array.from({ length: colCount }).map((_, ci) => {
              const cell = header[ci] ?? "";
              return (
                <th
                  key={ci}
                  style={{
                    minWidth: 100,
                    maxWidth: 320,
                    padding: "0 10px",
                    height: ROW_HEIGHT,
                    background: "var(--color-bg-tertiary)",
                    color: "var(--color-text-primary)",
                    fontWeight: 600,
                    textAlign: "left",
                    position: "sticky",
                    top: 0,
                    borderBottom: "1px solid var(--color-border-subtle)",
                    borderRight: "1px solid var(--color-bg-code-border)",
                    whiteSpace: "nowrap",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                  }}
                  title={cell}
                >
                  {cell || colPlaceholder(ci)}
                </th>
              );
            })}
          </tr>
        </thead>
        <tbody style={useVirtual ? { height: totalHeight, position: "relative" } : undefined}>
          {useVirtual ? (
            // Virtualised: render the visible window with a translate
            // offset so it lines up with the absolute totalHeight.
            <tr style={{ transform: `translateY(${startIdx * ROW_HEIGHT}px)`, height: 0 }}>
              <td colSpan={colCount + 1} style={{ padding: 0, border: 0 }}>
                <table
                  style={{
                    borderCollapse: "separate",
                    borderSpacing: 0,
                    width: "100%",
                  }}
                >
                  <tbody>
                    {window.map((row, ri) => (
                      <DataRow
                        key={startIdx + ri}
                        rowIdx={startIdx + ri}
                        row={row}
                        colCount={colCount}
                      />
                    ))}
                  </tbody>
                </table>
              </td>
            </tr>
          ) : (
            // Small sheet — render straight into the body, no offset
            window.map((row, ri) => (
              <DataRow key={ri} rowIdx={ri} row={row} colCount={colCount} />
            ))
          )}
        </tbody>
      </table>

      {data.length === 0 && (
        <div className="text-[12px] text-text-muted p-6 text-center">
          {header.length > 0 ? "没有匹配的数据行" : "工作表为空"}
        </div>
      )}
    </div>
  );
}

function DataRow({ rowIdx, row, colCount }: { rowIdx: number; row: string[]; colCount: number }) {
  return (
    <tr
      className="hover:bg-bg-hover"
      style={{ height: ROW_HEIGHT }}
    >
      <td
        className="sticky text-center text-text-faint font-mono text-[10.5px]"
        style={{
          left: 0,
          position: "sticky",
          width: 38,
          minWidth: 38,
          padding: "0 6px",
          background: "var(--color-bg-primary)",
          borderRight: "1px solid var(--color-border-subtle)",
        }}
      >
        {rowIdx + 2}
        {/* +2 because the first row is the header, and Excel rows are 1-indexed */}
      </td>
      {Array.from({ length: colCount }).map((_, ci) => {
        const cell = row[ci] ?? "";
        return (
          <td
            key={ci}
            className="text-text-primary"
            style={{
              minWidth: 100,
              maxWidth: 320,
              padding: "0 10px",
              borderRight: "1px solid var(--color-bg-code-border)",
              borderBottom: "1px solid var(--color-bg-code-border)",
              whiteSpace: "nowrap",
              overflow: "hidden",
              textOverflow: "ellipsis",
            }}
            title={cell}
          >
            {cell}
          </td>
        );
      })}
    </tr>
  );
}

/** When a sheet has fewer header cells than the widest data row,
 * we label the empty header cells "A", "B", "C", … so the user
 * still has something readable. Excel does the same. */
function colPlaceholder(idx: number): string {
  let s = "";
  let n = idx;
  do {
    s = String.fromCharCode(65 + (n % 26)) + s;
    n = Math.floor(n / 26) - 1;
  } while (n >= 0);
  return s;
}
