// Excel 工作簿编辑器（重量模式专属）——**表格本体就地编辑**。
//
// 用户诉求（2026-10-20）："打开 excel 默认直接就可以编辑，因为 office 办公
// 软件就是这样的"。所以这里渲染出来的就是一张能改的表：底色、字色、边框、
// 对齐、列宽、合并区都按源文件还原，点击格子直接改。
//
// **显示**走共享层的 `parseXlsx`（ExcelJS 解析，样式与格式化文本都还原，
// 与桌面端预览同一份代码路径）；**回写**在保存时才用 ExcelJS 重新载入原
// 文件，只改动用户真正动过的格子再导出——没动过的公式、样式、其它工作表
// 一律原样保留（不是「读成 JSON 再生成」那种全本重建）。
//
// 公式格显示计算结果（与 Excel 一致），悬停可见原公式；`=` 开头写成公式。
//
// 原文件永不覆盖：产物走 Rust 的 `write_bytes`（带 `safe_output_path` 守卫），
// 默认落 `<原名>.edited.xlsx`。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Save, FolderOpen, ChevronLeft, ChevronRight } from "lucide-react";
import {
  colName,
  colWidthPx,
  parseXlsx,
  rowHeightPx,
  styleToCss,
  type FileSource,
  type ParsedWorkbook,
  type SheetModel,
} from "@hermix/preview";
import { encodeBase64, editedName } from "./office";

interface SaveResult {
  path: string;
  bytes: number;
}

const ROW_HEADER_W = 48;
const DEFAULT_ROW_H = 22;
const DEFAULT_COL_W = 88;
const MIN_COL_W = 40;
const OVERSCAN = 8;

/** 编辑后的文本 → ExcelJS 单元格值。`=` 开头按公式写。 */
function parseCellText(text: string): string | number | { formula: string } {
  if (text.startsWith("=")) return { formula: text.slice(1) };
  const t = text.trim();
  if (t !== "" && /^-?\d+(\.\d+)?$/.test(t)) return Number(t);
  return text;
}

export function SheetEditor({
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
  const [book, setBook] = useState<ParsedWorkbook | null>(null);
  const [sheetIdx, setSheetIdx] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [active, setActive] = useState<{ r: number; c: number } | null>(null);

  /** 改动表放 ref（每次输入都写它），只有计数进 state 才触发重渲染。 */
  const changesRef = useRef(new Map<string, string>());
  const [changedCount, setChangedCount] = useState(0);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const r = await source.readBytes(path);
        if (!r.ok) {
          if (!cancelled) setError(r.message);
          return;
        }
        const parsed = await parseXlsx(r.bytes);
        if (cancelled) return;
        setBook(parsed);
        setSheetIdx(0);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [path, source]);

  const sheet = book?.sheets[Math.min(sheetIdx, Math.max(0, (book?.sheets.length ?? 1) - 1))];

  const onInput = useCallback(
    (r: number, c: number, text: string) => {
      const key = `${sheetIdx}:${r}:${c}`;
      const map = changesRef.current;
      const before = map.size;
      map.set(key, text);
      if (map.size !== before) setChangedCount(map.size);
    },
    [sheetIdx],
  );

  const valueOf = useCallback(
    (r: number, c: number): string => {
      const key = `${sheetIdx}:${r}:${c}`;
      const changed = changesRef.current.get(key);
      if (changed !== undefined) return changed;
      return sheet?.rows[r]?.[c]?.text ?? "";
    },
    [sheetIdx, sheet],
  );

  const doWrite = useCallback(
    async (outPath: string | null) => {
      const map = changesRef.current;
      if (map.size === 0) {
        setNote("没有修改可保存");
        return;
      }
      setBusy(true);
      setNote(null);
      try {
        const r = await source.readBytes(path);
        if (!r.ok) throw new Error(r.message);
        const { Workbook } = await import("exceljs");
        const wb = new Workbook();
        const buf = r.bytes.buffer.slice(
          r.bytes.byteOffset,
          r.bytes.byteOffset + r.bytes.byteLength,
        );
        // ExcelJS 的类型签名是 Node 的 Buffer；浏览器里给的是 ArrayBuffer。
        await wb.xlsx.load(buf as never);
        for (const [key, text] of map.entries()) {
          const [s, row, col] = key.split(":").map(Number);
          // **按数组下标取表**，不是 getWorksheet(id)：ExcelJS 的 id 从 1
          // 开始，getWorksheet(0) 返回 undefined，改动会被整条跳过（导出的
          // 文件看着成功，内容却原封不动）。下标与共享层 parseXlsx 的
          // wb.worksheets 顺序一致。
          const ws = wb.worksheets[s];
          if (!ws) continue;
          ws.getCell(row + 1, col + 1).value = parseCellText(text);
        }
        const out = await wb.xlsx.writeBuffer();
        const res = await invoke<SaveResult>("write_bytes", {
          srcPath: path,
          outPath,
          dataBase64: encodeBase64(new Uint8Array(out as ArrayBuffer)),
        });
        setNote(`已导出 ${res.path}（${Math.round(res.bytes / 1024)} KB）`);
      } catch (e) {
        setNote(`导出失败：${e instanceof Error ? e.message : String(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [path, source],
  );

  const doSaveAs = useCallback(async () => {
    setBusy(true);
    try {
      const out = await invoke<string | null>("image_save_dialog", {
        defaultName: editedName(name, "xlsx"),
        filters: [["Excel", ["xlsx"]]],
      });
      setBusy(false);
      if (!out) return;
      await doWrite(out);
    } catch (e) {
      setBusy(false);
      setNote(`另存失败：${e instanceof Error ? e.message : String(e)}`);
    }
  }, [name, doWrite]);

  const reveal = useCallback(async () => {
    if (!note) return;
    const m = note.match(/已导出 (.+?)（/);
    if (m) await invoke("viewer_reveal_in_explorer", { path: m[1] });
  }, [note]);

  const sheetCount = book?.sheets.length ?? 0;

  return (
    <div className="ed-editor">
      <div className="ed-toolbar">
        <span className="ed-toolbar-name" title={path}>
          {name}
        </span>
        {changedCount > 0 && <span className="ed-dirty">已改 {changedCount} 格</span>}
        <div className="toolbar-spacer" />
        {sheetCount > 1 && (
          <div className="sx-switch">
            <button
              type="button"
              className="sx-step"
              onClick={() => setSheetIdx((i) => (i - 1 + sheetCount) % sheetCount)}
              title="上一个工作表"
              aria-label="上一个工作表"
            >
              <ChevronLeft className="icon-sm" />
            </button>
            <span className="sx-sheet-name" title={sheet?.name}>
              {sheet?.name}
            </span>
            <button
              type="button"
              className="sx-step"
              onClick={() => setSheetIdx((i) => (i + 1) % sheetCount)}
              title="下一个工作表"
              aria-label="下一个工作表"
            >
              <ChevronRight className="icon-sm" />
            </button>
          </div>
        )}
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doWrite(null)}
          disabled={busy || changedCount === 0 || !!error}
          title="导出为新文件"
          aria-label="导出工作簿"
        >
          <Save className="icon-sm" /> 导出
        </button>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void doSaveAs()}
          disabled={busy || changedCount === 0 || !!error}
          title="另存为…"
          aria-label="另存工作簿"
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
        <div className="ed-panel sx-panel">
          {error ? (
            <div className="preview-state-center">
              <div className="preview-state-error">
                <p>{error}</p>
                <small>{path}</small>
              </div>
            </div>
          ) : !sheet ? (
            <div className="preview-state-center">正在解析工作簿…</div>
          ) : (
            <SheetGrid
              sheet={sheet}
              active={active}
              onActivate={setActive}
              valueOf={valueOf}
              onInput={onInput}
            />
          )}
        </div>

        <aside className="ed-side">
          <div className="sb-head">状态</div>
          <div className="ed-status">
            {sheet ? `${sheet.rowCount} 行 × ${sheet.colCount} 列` : "—"} · 已改 {changedCount} 格
          </div>
          <div className="ed-status">
            点格子即可编辑。`=` 开头写成公式；没动过的格子原样保留（含公式与格式）。
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

/**
 * 网格本体。
 *
 * 只对**行**做窗口化：真正会炸的是行数（几万行的表）。行高取源文件的值，
 * 窗口用「行高前缀和 + 二分」求——写死行高会在自定义行高的表上错位。
 */
function SheetGrid({
  sheet,
  active,
  onActivate,
  valueOf,
  onInput,
}: {
  sheet: SheetModel;
  active: { r: number; c: number } | null;
  onActivate: (pos: { r: number; c: number } | null) => void;
  valueOf: (r: number, c: number) => string;
  onInput: (r: number, c: number, text: string) => void;
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

  const rowHeights = useMemo(
    () =>
      Array.from({ length: sheet.rowCount }, (_, r) => {
        const h = sheet.rowHeights[r];
        return h ? (rowHeightPx(h) ?? DEFAULT_ROW_H) : DEFAULT_ROW_H;
      }),
    [sheet],
  );

  const offsets = useMemo(() => {
    const acc = [0];
    for (const h of rowHeights) acc.push(acc[acc.length - 1] + h);
    return acc;
  }, [rowHeights]);

  const win = useMemo(() => {
    const total = offsets[offsets.length - 1] ?? 0;
    let lo = 0;
    let hi = offsets.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if ((offsets[mid] ?? 0) <= scrollTop) lo = mid;
      else hi = mid - 1;
    }
    const start = Math.max(0, lo - OVERSCAN);
    let end = lo;
    while (end < offsets.length - 1 && (offsets[end] ?? 0) < scrollTop + viewport) end++;
    end = Math.min(offsets.length, end + 1 + OVERSCAN);
    return {
      start,
      end,
      topPad: offsets[start] ?? 0,
      bottomPad: Math.max(0, total - (offsets[end] ?? total)),
    };
  }, [offsets, scrollTop, viewport]);

  const widths = useMemo(
    () =>
      Array.from({ length: sheet.colCount }, (_, c) => {
        const w = sheet.colWidths[c];
        return w ? Math.max(MIN_COL_W, colWidthPx(w) ?? DEFAULT_COL_W) : DEFAULT_COL_W;
      }),
    [sheet],
  );

  /** 合并区：主格记跨度，被覆盖的格跳过（否则同一段文字会重复出现）。 */
  const merges = useMemo(() => {
    const spans = new Map<string, { rs: number; cs: number }>();
    const covered = new Set<string>();
    for (const g of sheet.merges) {
      spans.set(`${g.row}:${g.col}`, { rs: g.rowSpan, cs: g.colSpan });
      for (let r = g.row; r < g.row + g.rowSpan; r++) {
        for (let c = g.col; c < g.col + g.colSpan; c++) {
          if (r !== g.row || c !== g.col) covered.add(`${r}:${c}`);
        }
      }
    }
    return { spans, covered };
  }, [sheet.merges]);

  const move = useCallback(
    (dr: number, dc: number) => {
      if (!active) return;
      const r = Math.min(Math.max(0, active.r + dr), Math.max(0, sheet.rowCount - 1));
      const c = Math.min(Math.max(0, active.c + dc), Math.max(0, sheet.colCount - 1));
      onActivate({ r, c });
    },
    [active, sheet.rowCount, sheet.colCount, onActivate],
  );

  const rows = useMemo(() => {
    const out: number[] = [];
    for (let r = win.start; r < win.end; r++) out.push(r);
    return out;
  }, [win.start, win.end]);

  return (
    <div
      className="sx-scroll"
      ref={scrollRef}
      onScroll={(e) => setScrollTop(e.currentTarget.scrollTop)}
    >
      <table className="sx-table">
        <colgroup>
          <col style={{ width: ROW_HEADER_W }} />
          {widths.map((w, i) => (
            <col key={i} style={{ width: w }} />
          ))}
        </colgroup>
        <thead>
          <tr>
            <th className="sx-corner" scope="col" />
            {Array.from({ length: sheet.colCount }, (_, c) => (
              <th key={c} scope="col" className="sx-col-head">
                {colName(c)}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {win.topPad > 0 && (
            <tr style={{ height: win.topPad }} aria-hidden="true">
              <td colSpan={sheet.colCount + 1} className="sx-pad" />
            </tr>
          )}
          {rows.map((r) => (
            <tr key={r} style={{ height: rowHeights[r] ?? DEFAULT_ROW_H }}>
              <th scope="row" className="sx-row-head">
                {r + 1}
              </th>
              {Array.from({ length: sheet.colCount }, (_, c) => {
                if (merges.covered.has(`${r}:${c}`)) return null;
                const cell = sheet.rows[r]?.[c] ?? null;
                const isActive = active?.r === r && active?.c === c;
                const span = merges.spans.get(`${r}:${c}`);
                const css: CSSProperties = {
                  ...styleToCss(cell?.style),
                  ...(cell?.style?.color ? {} : { color: "var(--color-text-primary)" }),
                  ...(cell?.style?.valign ? {} : { verticalAlign: "bottom" as const }),
                };
                return (
                  <td
                    key={c}
                    className={isActive ? "sx-cell is-active" : "sx-cell"}
                    style={css}
                    colSpan={span?.cs}
                    rowSpan={span?.rs}
                    title={cell?.formula ? `= ${cell.formula}` : undefined}
                    onClick={() => onActivate({ r, c })}
                  >
                    {isActive ? (
                      <input
                        className="sx-input"
                        defaultValue={valueOf(r, c)}
                        autoFocus
                        spellCheck={false}
                        aria-label={`第 ${r + 1} 行 第 ${c + 1} 列`}
                        onChange={(e) => onInput(r, c, e.currentTarget.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter") {
                            e.preventDefault();
                            move(1, 0);
                          } else if (e.key === "Tab") {
                            e.preventDefault();
                            move(0, e.shiftKey ? -1 : 1);
                          } else if (e.key === "Escape") {
                            e.preventDefault();
                            onActivate(null);
                          }
                        }}
                      />
                    ) : (
                      <span className="sx-text">{cell?.text}</span>
                    )}
                  </td>
                );
              })}
            </tr>
          ))}
          {win.bottomPad > 0 && (
            <tr style={{ height: win.bottomPad }} aria-hidden="true">
              <td colSpan={sheet.colCount + 1} className="sx-pad" />
            </tr>
          )}
          {sheet.rowCount === 0 && (
            <tr>
              <td colSpan={sheet.colCount + 1} className="sx-empty">
                工作表为空
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}
