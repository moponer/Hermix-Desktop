// Frontend parser for the artifact reading surface. Replaces the
// `officecli`-based preview path (which returned raw text-mode HTML
// for spreadsheets) by parsing Excel/CSV/docx in the browser via
// SheetJS + PapaParse. The downstream surface is a single
// `PreviewModel` discriminated union so `RightSidebar` doesn't need
// to know whether the file was `.xlsx` or `.csv` — the parsed
// rows/columns back a real <table> with sort/search/export, which a
// pre-rendered HTML blob couldn't offer.

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

// Dispatches to the right parser by file extension. The caller must
// hand us bytes it already read off disk (via `tryReadArtifact`) —
// we don't deal with paths here.
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
    return { kind: "binary", size: bytes.byteLength };
  } catch (e) {
    return { kind: "error", message: String(e) };
  }
}

function parseWorkbook(bytes: Uint8Array): PreviewModel {
  const wb = XLSX.read(bytes, { type: "array", cellDates: true });

  const sheets = wb.SheetNames.map((name) => {
    const ws = wb.Sheets[name];
    // `raw: false` returns formatted strings instead of raw serial
    // numbers, so date / numeric cells render as humans expect rather
    // than as Excel epoch integers.
    const aoa = XLSX.utils.sheet_to_json<string[]>(ws, {
      header: 1,
      raw: false,
      defval: "",
    });
    // SheetJS returns rows of varying length when the sheet has
    // missing trailing cells — pad to the sheet-wide max so every
    // row aligns in the rendered <table>.
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

function parseCsv(bytes: Uint8Array, isTsv: boolean): PreviewModel {
  const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  // Strip BOM — PapaParse can produce a leading null-char in the
  // first header when the BOM is left in place.
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

function parseDocx(_bytes: Uint8Array): PreviewModel {
  // mammoth (≈50 KB gzipped) isn't bundled — .docx opens via the
  // backend's `render_office_preview` fallback, so the in-browser
  // path here returns a clear error directing users there.
  return {
    kind: "error",
    message:
      "前端 .docx 渲染器尚未启用（计划中），目前请用 Word/WPS 另存为 .xlsx 或 .pdf 后查看。",
  };
}

function parseText(bytes: Uint8Array): PreviewModel {
  const text = new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  // Drop a single trailing newline so the count matches RightSidebar's
  // status bar (which doesn't count a trailing empty line).
  const lines = text.split("\n");
  if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
  return { kind: "text", content: text, lineCount: lines.length, byteSize: bytes.byteLength };
}

// Re-formats a parsed sheet as CSV text (used by the "导出 CSV"
// button — always operates on the active sheet).
export function sheetToCsv(sheet: { rows: string[][]; name: string }): string {
  return Papa.unparse(
    sheet.rows.map((r) => r.map((cell) => cell ?? "")),
    { newline: "\r\n" },
  );
}
