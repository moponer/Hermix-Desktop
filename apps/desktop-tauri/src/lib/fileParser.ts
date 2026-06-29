/**
 * Frontend file parser for the artifact reading surface.
 *
 * Why this exists (2026-06-22): the previous office preview
 * (`render_office_preview` → `officecli` → raw HTML) was a black box
 * that produced the broken "Plain text" view in the screenshot —
 * officecli either wasn't installed or returned text-mode output
 * rather than a real table. We don't want every desktop release to
 * depend on a sidecar `officecli` binary.
 *
 * Instead we now parse Excel/CSV/docx *in the browser* using
 * SheetJS + PapaParse + mammoth. The advantages:
 *
 * 1. No backend install step (no LibreOffice, no officecli)
 * 2. Lower-latency preview (no subprocess spawn)
 * 3. The parsed rows/columns let us build a real <table> with
 *    sortable headers, in-table search, sheet switcher, CSV export
 *    — features we couldn't easily layer on a pre-rendered HTML blob
 *
 * The downstream surface is a single `PreviewModel` discriminated
 * union so the panel in RightSidebar doesn't need to know whether
 * the file was a `.xlsx` or a `.csv`.
 */

import * as XLSX from "xlsx";
import Papa from "papaparse";

export type PreviewModel =
  | {
      kind: "spreadsheet";
      /** Sheets keyed by name; insertion order preserved */
      sheets: Array<{ name: string; rows: string[][]; rowCount: number; colCount: number }>;
      activeSheet: number;
    }
  | { kind: "csv"; rows: string[][]; rowCount: number; colCount: number }
  | { kind: "docx"; html: string; plainText: string }
  | { kind: "text"; content: string; lineCount: number; byteSize: number }
  | { kind: "binary"; size: number }
  | { kind: "error"; message: string };

/** Detect the parse path from the file extension. The caller must
 * hand us bytes it already read off disk (via the existing
 * `tryReadArtifact` helper), so we don't deal with paths here. */
export function parseByExtension(
  fileName: string,
  bytes: Uint8Array,
): PreviewModel {
  const lower = fileName.toLowerCase();

  try {
    if (lower.endsWith(".xlsx") || lower.endsWith(".xls") || lower.endsWith(".xlsm")) {
      return parseWorkbook(bytes);
    }
    if (lower.endsWith(".csv") || lower.endsWith(".tsv")) {
      return parseCsv(bytes, lower.endsWith(".tsv"));
    }
    if (lower.endsWith(".docx")) {
      return parseDocx(bytes);
    }
    if (lower.endsWith(".doc")) {
      return {
        kind: "error",
        message:
          "旧版 .doc (二进制) 不支持，请用 Word 另存为 .docx 后重试。",
      };
    }
    if (
      lower.endsWith(".txt") ||
      lower.endsWith(".md") ||
      lower.endsWith(".json") ||
      lower.endsWith(".yaml") ||
      lower.endsWith(".yml") ||
      lower.endsWith(".xml") ||
      lower.endsWith(".html") ||
      lower.endsWith(".htm") ||
      lower.endsWith(".log") ||
      lower.endsWith(".ini") ||
      lower.endsWith(".toml")
    ) {
      return parseText(bytes);
    }
    // Unknown — treat as binary so the panel can show a download prompt
    return { kind: "binary", size: bytes.byteLength };
  } catch (e) {
    return { kind: "error", message: String(e) };
  }
}

/* ────────────────────────────────────────────────────────────────────
   Spreadsheet (xlsx / xls)
   ──────────────────────────────────────────────────────────────────── */

function parseWorkbook(bytes: Uint8Array): PreviewModel {
  // SheetJS accepts ArrayBuffer / Uint8Array; `bookType: undefined`
  // lets it sniff between xls and xlsx on its own.
  const wb = XLSX.read(bytes, { type: "array", cellDates: true });

  const sheets = wb.SheetNames.map((name) => {
    const ws = wb.Sheets[name];
    // header: 1 → AOA (array of arrays), raw: false → formatted
    // strings (good enough for human display). We *don't* apply
    // raw: true because date cells and number cells would dump
    // ugly serial numbers at the user.
    const aoa = XLSX.utils.sheet_to_json<string[]>(ws, {
      header: 1,
      raw: false,
      defval: "",
    });
    // Pad ragged rows: SheetJS returns rows of different lengths
    // when the underlying sheet has missing trailing cells. We
    // normalise by widening to the max colCount across the sheet.
    const colCount = aoa.reduce((m, r) => Math.max(m, r.length), 0);
    const rows = aoa.map((r) => {
      if (r.length === colCount) return r;
      const padded = r.slice();
      while (padded.length < colCount) padded.push("");
      return padded;
    });
    return { name, rows, rowCount: rows.length, colCount };
  });

  if (sheets.length === 0) {
    return { kind: "error", message: "工作簿中没有可读取的 Sheet" };
  }
  return { kind: "spreadsheet", sheets, activeSheet: 0 };
}

/* ────────────────────────────────────────────────────────────────────
   CSV / TSV
   ──────────────────────────────────────────────────────────────────── */

function parseCsv(bytes: Uint8Array, isTsv: boolean): PreviewModel {
  const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  // Strip BOM — PapaParse can't always see past it and would produce
  // a leading null-char in the first header.
  const clean = text.replace(/^\uFEFF/, "");
  const result = Papa.parse<string[]>(clean, {
    delimiter: isTsv ? "\t" : "",
    skipEmptyLines: "greedy",
  });
  const rows = result.data;
  if (rows.length === 0) {
    return { kind: "csv", rows: [[]], rowCount: 0, colCount: 0 };
  }
  const colCount = rows.reduce((m, r) => Math.max(m, r.length), 0);
  return { kind: "csv", rows, rowCount: rows.length, colCount };
}

/* ────────────────────────────────────────────────────────────────────
   docx — keep this as a *plain-text* fallback for now
   ──────────────────────────────────────────────────────────────────── */

function parseDocx(_bytes: Uint8Array): PreviewModel {
  // We deliberately do NOT bundle mammoth (~50KB gzipped) for a
  // minimal first cut. docx is rare in this product (the .xlsx and
  // .docm flows dominate the use case), and our backend already
  // ships a `render_office_preview` fallback for it. So if a user
  // really opens a .docx we hand them a clear error directing them
  // back to LibreOffice until a future commit adds mammoth.
  return {
    kind: "error",
    message:
      "前端 .docx 渲染器尚未启用（计划中），目前请用 Word/WPS 另存为 .xlsx 或 .pdf 后查看。",
  };
}

/* ────────────────────────────────────────────────────────────────────
   Text
   ──────────────────────────────────────────────────────────────────── */

function parseText(bytes: Uint8Array): PreviewModel {
  const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  // Mirror the line-count helper from RightSidebar so the bottom
  // status bar stays consistent.
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return { kind: "text", content: text, lineCount: lines.length, byteSize: bytes.byteLength };
}

/* ────────────────────────────────────────────────────────────────────
   Helpers
   ──────────────────────────────────────────────────────────────────── */

/** Re-format a parsed spreadsheet as CSV text — used by the
 * "导出 CSV" button. Always uses the *active* sheet. */
export function sheetToCsv(sheet: { rows: string[][]; name: string }): string {
  return Papa.unparse(
    sheet.rows.map((r) => r.map((cell) => cell ?? "")),
    { newline: "\r\n" },
  );
}
