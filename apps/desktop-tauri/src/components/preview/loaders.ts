// 每种预览载荷的加载器。副作用（Tauri IO、解析、外部转换）全部集中在
// 这一层，渲染层拿到的永远是已经成形的数据。
//
// 关键约束：加载器可以返回与请求种类不同的载荷（排版视图失败退到
// Markdown），所以调用方应以 `payload.kind` 为准，而不是请求时的 kind。

import {
  tryConvertDocumentToMarkdown,
  tryReadArtifact,
  tryReadArtifactBytes,
  tryRenderOfficePreview,
} from "../../utils/artifactReader";
import { parseByExtension } from "../../lib/fileParser";
import {
  IMAGE_BYTE_LIMIT,
  IMAGE_EXT_MIME,
  detectLanguage,
  isMarkdownExt,
} from "../rightSidebarUtils";
import type { LoadOutcome, LoadRequest, PreviewKind, PreviewLoader } from "./types";

/// 排版视图失败原因 → 人话。原始错误是 Rust 侧字符串，直接给用户看
/// 会出现 "officecli binary not found at F:\...\binaries\..." 这种路径噪音。
function friendlyOfficeError(raw: string): string {
  if (/unsupported file type/i.test(raw)) return "不支持的 Office 文件格式";
  if (/officecli binary not found/i.test(raw)) {
    return "OfficeCLI 渲染引擎未找到，请检查应用安装完整性";
  }
  return raw;
}

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

/** 读取字节的结果：成功带字节，失败带人话错误。 */
type BytesResult =
  | { ok: true; bytes: Uint8Array; resolvedPath: string | null }
  | { ok: false; message: string };

/** 读取字节并在超限时给出统一文案。 */
async function readBytesWithin(req: LoadRequest, limit: number, label: string): Promise<BytesResult> {
  const res = await tryReadArtifactBytes(req.path, req.workspaceRoot);
  if (res.kind === "error") {
    return { ok: false, message: friendlyReadError(res.message) };
  }
  if (res.size > limit) {
    return {
      ok: false,
      message: `${label}过大（${humanMb(res.size)}），预览上限 ${humanMb(limit)}。`,
    };
  }
  return { ok: true, bytes: res.bytes, resolvedPath: res.resolvedPath };
}

const imageLoader: PreviewLoader = async (req) => {
  const r = await readBytesWithin(req, IMAGE_BYTE_LIMIT, "图片");
  if (!r.ok) return r;
  const ext = req.name.split(".").pop()?.toLowerCase() ?? "";
  const mime = IMAGE_EXT_MIME[ext] ?? "application/octet-stream";
  return {
    ok: true,
    resolvedPath: r.resolvedPath,
    payload: { kind: "image", bytes: r.bytes, mime, size: r.bytes.byteLength },
  };
};

/** OOXML 字节加载器：docx / pptx 都只吃原始字节，由渲染层各自解析。 */
function byteLoader(kind: "pdf" | "docx" | "pptx", limit: number, label: string): PreviewLoader {
  return async (req) => {
    const r = await readBytesWithin(req, limit, label);
    if (!r.ok) return r;
    const bytes = r.bytes;
    if (kind === "pdf") {
      return { ok: true, resolvedPath: r.resolvedPath, payload: { kind: "pdf", bytes, fileName: req.name } };
    }
    if (kind === "docx") {
      return { ok: true, resolvedPath: r.resolvedPath, payload: { kind: "docx", bytes, fileName: req.name } };
    }
    return { ok: true, resolvedPath: r.resolvedPath, payload: { kind: "pptx", bytes, fileName: req.name } };
  };
}

const markdownLoader: PreviewLoader = async (req) => {
  // 两个来源：.md 文件直读，或 Office / PDF 切到文本视图后由 markitdown
  // 转换而来。按扩展名区分，调用方只需知道 kind === "markdown"。
  if (isMarkdownExt(req.name)) {
    const res = await tryReadArtifact(req.path, req.workspaceRoot);
    if (res.kind === "error") return { ok: false, message: friendlyReadError(res.message) };
    return {
      ok: true,
      resolvedPath: res.resolvedPath,
      payload: {
        kind: "markdown",
        content: res.kind === "text" ? res.content : "",
        fileName: req.name,
        truncated: false,
      },
    };
  }
  return documentMarkdownLoader(req);
};

/** Office 文档转 Markdown（markitdown sidecar）。 */
function documentMarkdownLoader(req: LoadRequest): Promise<LoadOutcome> {
  return tryConvertDocumentToMarkdown(req.path, req.workspaceRoot).then((md) => {
    if ("error" in md) return { ok: false, message: friendlyReadError(md.error) };
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

// 老二进制 Office 格式（doc / xls / ppt）没有纯前端解析器，仍走 officecli。
// 它是唯一保留的外部进程依赖，失败时退到 markitdown 的 Markdown 表示。
const officeLoader: PreviewLoader = async (req) => {
  const res = await tryRenderOfficePreview(req.path, req.workspaceRoot);
  if (!("error" in res)) {
    return {
      ok: true,
      resolvedPath: res.resolvedPath,
      payload: { kind: "office", htmlContent: res.htmlContent, fileName: res.fileName },
    };
  }
  const fallback = await documentMarkdownLoader(req);
  if (fallback.ok) return fallback;
  return {
    ok: false,
    message: `排版视图失败：${friendlyOfficeError(res.error)}。文本视图也失败：${fallback.message}`,
  };
};

const spreadsheetLoader: PreviewLoader = async ({ name, path, workspaceRoot }) => {
  const res = await tryReadArtifactBytes(path, workspaceRoot);
  if (res.kind === "error") return { ok: false, message: friendlyReadError(res.message) };
  const model = parseByExtension(name, res.bytes);
  if (model.kind === "error") return { ok: false, message: model.message };
  if (model.kind === "binary") {
    return { ok: true, resolvedPath: res.resolvedPath, payload: { kind: "text", content: "" } };
  }
  return {
    ok: true,
    resolvedPath: res.resolvedPath,
    payload: { kind: "spreadsheet", model, fileName: name },
  };
};

const textLoader: PreviewLoader = async ({ name, path, workspaceRoot }) => {
  const res = await tryReadArtifact(path, workspaceRoot);
  if (res.kind === "error") return { ok: false, message: friendlyReadError(res.message) };
  if (res.kind === "binary") {
    return { ok: false, message: `二进制文件（${humanMb(res.size)}），不支持文本预览。` };
  }
  const content = res.kind === "text" ? res.content : "";
  return {
    ok: true,
    resolvedPath: res.resolvedPath,
    payload: { kind: "code", content, language: detectLanguage(name) },
  };
};

export const LOADERS: Record<PreviewKind, PreviewLoader> = {
  image: imageLoader,
  pdf: byteLoader("pdf", 50 * 1024 * 1024, "PDF"),
  markdown: markdownLoader,
  docx: byteLoader("docx", 50 * 1024 * 1024, "文档"),
  pptx: byteLoader("pptx", 100 * 1024 * 1024, "演示文稿"),
  office: officeLoader,
  spreadsheet: spreadsheetLoader,
  code: textLoader,
  text: textLoader,
};

export { documentMarkdownLoader };
