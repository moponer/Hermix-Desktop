// 每种预览载荷的加载器。副作用（IO、解析、外部转换）全部集中在这一层，
// 渲染层拿到的永远是已经成形的数据。
//
// 关键约束：
//   1. 加载器接受一份 FileSource —— desktop 走 artifact reader（带产物根
//      白名单 + 祖先回溯），viewer 走 Tauri `viewer_read_file`。二者对
//      "读不出来" 的错误形式不同，loader 统一收敛成 `{ ok, message }`。
//   2. 加载器可以返回与请求种类不同的载荷（排版视图失败退到 Markdown），
//      所以调用方应以 `payload.kind` 为准，而不是请求时的 kind。

import type { FileSource } from "./lib/fileSource";
import {
  IMAGE_BYTE_LIMIT,
  IMAGE_EXT_MIME,
  detectLanguage,
  isMarkdownExt,
} from "./lib/previewUtils";
import { parseDelimited, parseXls, parseXlsx } from "./lib/workbook";
import type { LoadOutcome, LoadRequest, PreviewKind, PreviewLoader } from "./types";

/// 后端拒绝读取的原因 → 人话。后端只抛英文字符串，这里收敛成用户能懂的话。
function friendlyReadError(raw: string): string {
  if (/cannot resolve path|cannot find the path|系统找不到指定的文件|os error 2/i.test(raw)) {
    return "文件不存在或已移动。";
  }
  if (/outside the allowed artifact roots/i.test(raw)) {
    return "该路径不在允许的产物根目录内（出于安全考虑被拒绝）。";
  }
  if (/file too large/i.test(raw)) {
    return "文件过大，仅支持预览 ≤ 2 MiB 的文本文件。";
  }
  return raw;
}

function humanMb(n: number): string {
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** 读取字节并在超限时给出统一文案。 */
async function readBytesWithin(
  source: FileSource,
  path: string,
  limit: number,
  label: string,
): Promise<
  | { ok: true; bytes: Uint8Array; size: number; resolvedPath: string | null }
  | { ok: false; message: string }
> {
  const res = await source.readBytes(path);
  if (!res.ok) return { ok: false, message: friendlyReadError(res.message) };
  if (res.size > limit) {
    return {
      ok: false,
      message: `${label}过大（${humanMb(res.size)}），预览上限 ${humanMb(limit)}。`,
    };
  }
  return { ok: true, bytes: res.bytes, size: res.size, resolvedPath: res.resolvedPath };
}

const imageLoader =
  (source: FileSource): PreviewLoader =>
  async (req) => {
    const r = await readBytesWithin(source, req.path, IMAGE_BYTE_LIMIT, "图片");
    if (!r.ok) return r;
    const ext = req.name.split(".").pop()?.toLowerCase() ?? "";
    const mime = IMAGE_EXT_MIME[ext] ?? "application/octet-stream";
    return {
      ok: true,
      resolvedPath: r.resolvedPath,
      payload: { kind: "image", bytes: r.bytes, mime, size: r.size },
    };
  };

/** OOXML 字节加载器：docx / pptx 都只吃原始字节，由渲染层各自解析。 */
function byteLoader(
  source: FileSource,
  kind: "pdf" | "docx" | "pptx",
  limit: number,
  label: string,
): PreviewLoader {
  return async (req) => {
    const r = await readBytesWithin(source, req.path, limit, label);
    if (!r.ok) return r;
    if (kind === "pdf") {
      return { ok: true, resolvedPath: r.resolvedPath, payload: { kind: "pdf", bytes: r.bytes, fileName: req.name } };
    }
    if (kind === "docx") {
      return { ok: true, resolvedPath: r.resolvedPath, payload: { kind: "docx", bytes: r.bytes, fileName: req.name } };
    }
    return { ok: true, resolvedPath: r.resolvedPath, payload: { kind: "pptx", bytes: r.bytes, fileName: req.name } };
  };
}

const markdownLoader =
  (source: FileSource): PreviewLoader =>
  async (req) => {
    // 两个来源：.md 文件直读，或 Office / PDF 切到文本视图后由 markitdown
    // 转换而来。按扩展名区分，调用方只需知道 kind === "markdown"。
    if (isMarkdownExt(req.name)) {
      const res = await source.readText(req.path);
      if (res.ok && "isBinary" in res && res.isBinary) {
        return { ok: false, message: `二进制文件（${humanMb(res.size)}），不是 Markdown。` };
      }
      if (!res.ok) return { ok: false, message: friendlyReadError(res.message) };
      if (res.ok && "isEmpty" in res && res.isEmpty) {
        return {
          ok: true,
          resolvedPath: res.resolvedPath,
          payload: { kind: "markdown", content: "", fileName: req.name, truncated: false },
        };
      }
      if (res.ok && "content" in res) {
        return {
          ok: true,
          resolvedPath: res.resolvedPath,
          payload: { kind: "markdown", content: res.content, fileName: req.name, truncated: false },
        };
      }
      return { ok: false, message: "读取失败" };
    }
    return documentMarkdownLoader(source, req);
  };

/** Office 文档转 Markdown（markitdown sidecar）。 */
function documentMarkdownLoader(source: FileSource, req: LoadRequest): Promise<LoadOutcome> {
  return source.readMarkdown(req.path).then((md) => {
    if (!md.ok) return { ok: false, message: friendlyReadError(md.message) };
    return {
      ok: true,
      resolvedPath: md.resolvedPath,
      payload: {
        kind: "markdown",
        content: md.markdown,
        fileName: md.fileName || req.name,
        truncated: md.truncated,
      },
    };
  });
}

/** 网页源码。切到文本视图时同一个读取器再被 code 视图用一次。 */
const htmlLoader =
  (source: FileSource): PreviewLoader =>
  async (req) => {
    const res = await source.readText(req.path);
    if (!res.ok) return { ok: false, message: friendlyReadError(res.message) };
    if (res.ok && "isBinary" in res && res.isBinary) {
      return { ok: false, message: `二进制文件（${humanMb(res.size)}），不是可渲染的网页。` };
    }
    if (res.ok && "isEmpty" in res && res.isEmpty) {
      return {
        ok: true,
        resolvedPath: res.resolvedPath,
        payload: { kind: "html", content: "", fileName: req.name, resolvedPath: res.resolvedPath },
      };
    }
    if (res.ok && "content" in res) {
      return {
        ok: true,
        resolvedPath: res.resolvedPath,
        payload: {
          kind: "html",
          content: res.content,
          fileName: req.name,
          resolvedPath: res.resolvedPath,
        },
      };
    }
    return { ok: false, message: "读取失败" };
  };

/**
 * 表格：按扩展名分派到唯一能读该格式的引擎。
 *   xlsx/xlsm → ExcelJS（唯一能读回 OOXML 样式表的引擎）
 *   xls       → SheetJS（ExcelJS 不支持 BIFF；样式读不回来，UI 如实标注）
 *   csv/tsv   → PapaParse
 * 引擎全部动态 import，打开 Markdown 时不为表格付下载成本。
 */
const spreadsheetLoader =
  (source: FileSource): PreviewLoader =>
  async ({ name, path }) => {
    const res = await source.readBytes(path);
    if (!res.ok) return { ok: false, message: friendlyReadError(res.message) };
    const ext = name.split(".").pop()?.toLowerCase() ?? "";
    try {
      const model =
        ext === "xls" ? await parseXls(res.bytes) : ext === "csv" || ext === "tsv"
          ? await parseDelimited(res.bytes)
          : await parseXlsx(res.bytes);
      return { ok: true, resolvedPath: res.resolvedPath, payload: { kind: "spreadsheet", model, fileName: name } };
    } catch (e) {
      return { ok: false, message: `表格解析失败：${String(e)}` };
    }
  };

const textLoader =
  (source: FileSource): PreviewLoader =>
  async ({ name, path }) => {
    const res = await source.readText(path);
    if (!res.ok) return { ok: false, message: friendlyReadError(res.message) };
    if (res.ok && "isBinary" in res && res.isBinary) {
      return { ok: false, message: `二进制文件（${humanMb(res.size)}），不支持文本预览。` };
    }
    if (res.ok && "isEmpty" in res && res.isEmpty) {
      return {
        ok: true,
        resolvedPath: res.resolvedPath,
        payload: { kind: "code", content: "", language: detectLanguage(name) },
      };
    }
    if (res.ok && "content" in res) {
      return {
        ok: true,
        resolvedPath: res.resolvedPath,
        payload: { kind: "code", content: res.content, language: detectLanguage(name) },
      };
    }
    return { ok: false, message: "读取失败" };
  };

export function buildLoaders(source: FileSource): Record<PreviewKind, PreviewLoader> {
  return {
    image: imageLoader(source),
    pdf: byteLoader(source, "pdf", 50 * 1024 * 1024, "PDF"),
    markdown: markdownLoader(source),
    html: htmlLoader(source),
    docx: byteLoader(source, "docx", 50 * 1024 * 1024, "文档"),
    pptx: byteLoader(source, "pptx", 100 * 1024 * 1024, "演示文稿"),
    spreadsheet: spreadsheetLoader(source),
    code: textLoader(source),
    text: textLoader(source),
  };
}

/** 用于测试 / 独立调用的便利入口。 */
export async function loadPreview(
  req: LoadRequest,
  source: FileSource,
): Promise<LoadOutcome> {
  const loaders = buildLoaders(source);
  const loader = loaders[req.kind];
  return loader(req);
}

export type { FileSource } from "./lib/fileSource";
