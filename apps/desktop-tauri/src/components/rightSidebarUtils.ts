// ── Pure helpers for the right sidebar tabs ─────────────
// 纯函数 / 常量：标签管理、格式化、扩展名与语言检测。
// 不依赖 React，可被 ComputerTab / ProductsTab / PreviewPanel 共用。
//
// 扩展名分流那批纯函数已迁到 @hermix/preview（previewUtils）—— 这里
// re-export 维持既有 import 路径，实现只剩一份；留在本文件的是右栏 UI
// 关注的事（标签、格式化、面包屑）。
import type { ElementType } from "react";
import {
  FolderOpen, FileCode, FileText, FileJson, FileImage, FileType2,
} from "lucide-react";
import type { ArtifactKind } from "../utils/artifacts";
import type { ParsedWorkbook } from "@hermix/preview";
import {
  detectLanguage,
  isHtmlExt,
  isImageExt,
  isMarkdownExt,
} from "@hermix/preview";

export {
  IMAGE_EXT_MIME,
  IMAGE_BYTE_LIMIT,
  isImageExt,
  isMarkdownExt,
  isPdfExt,
  isHtmlExt,
  detectLanguage,
  FRONTEND_PARSE_EXTS,
  officeExtGroup,
  needsFrontendParse,
  type OfficeExtGroup,
} from "@hermix/preview";

// ── Tab filters (tag management) ────────────────────────
export const FILTERS: Array<{ key: "all" | ArtifactKind; label: string; icon: ElementType }> = [
  { key: "all", label: "全部", icon: FolderOpen },
  { key: "code", label: "代码", icon: FileCode },
  { key: "doc", label: "文档", icon: FileText },
  { key: "data", label: "数据", icon: FileJson },
  { key: "image", label: "图片", icon: FileImage },
  { key: "office", label: "Office", icon: FileType2 },
];

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

export function totalRows(model: ParsedWorkbook): number {
  return model.sheets.reduce((s, sh) => s + sh.rowCount, 0);
}

export function modelLabel(model: ParsedWorkbook): string {
  return `${model.sheets.length} 个工作表`;
}

export const LANG_LABELS: Record<string, string> = {
  typescript: "TypeScript",
  tsx: "TSX",
  javascript: "JavaScript",
  jsx: "JSX",
  rust: "Rust",
  python: "Python 3",
  go: "Go",
  java: "Java",
  kotlin: "Kotlin",
  c: "C",
  cpp: "C++",
  csharp: "C#",
  objc: "Objective-C",
  ruby: "Ruby",
  php: "PHP",
  bash: "Bash",
  shell: "Shell",
  powershell: "PowerShell",
  batch: "Batch",
  css: "CSS",
  scss: "SCSS",
  less: "Less",
  html: "HTML",
  markup: "HTML/XML",
  json: "JSON",
  json5: "JSON5",
  jsonp: "JSONP",
  yaml: "YAML",
  xml: "XML",
  ini: "INI",
  toml: "TOML",
  properties: "Properties",
  sql: "SQL",
  lua: "Lua",
  swift: "Swift",
  dart: "Dart",
  scala: "Scala",
  elixir: "Elixir",
  haskell: "Haskell",
  nim: "Nim",
  zig: "Zig",
  julia: "Julia",
  r: "R",
  solidity: "Solidity",
  protobuf: "Protobuf",
  graphql: "GraphQL",
  nginx: "Nginx",
  docker: "Dockerfile",
  makefile: "Makefile",
  cmake: "CMake",
  ignore: "Git Ignore",
  diff: "Diff",
  http: "HTTP",
  regex: "Regex",
  vim: "Vim",
  markdown: "Markdown",
  latex: "LaTeX",
  asciidoc: "AsciiDoc",
  rest: "reStructuredText",
  wiki: "Wiki",
  pug: "Pug",
  handlebars: "Handlebars",
  csv: "CSV",
  mermaid: "Mermaid",
  git: "Git",
  text: "Text",
};

/// 状态栏 / 头部显示的类型标签。代码语言走 LANG_LABELS，其余按扩展名。
const EXT_TYPE_LABELS: Record<string, string> = {
  pdf: "PDF",
  docx: "Word", doc: "Word", odt: "Word", rtf: "RTF",
  xlsx: "Excel", xls: "Excel", ods: "Spreadsheet",
  csv: "CSV", tsv: "TSV",
  pptx: "PowerPoint", ppt: "PowerPoint", odp: "Presentation",
  png: "PNG", jpg: "JPEG", jpeg: "JPEG", gif: "GIF",
  webp: "WebP", bmp: "BMP", ico: "Icon", svg: "SVG",
  mp3: "Audio", wav: "Audio", mp4: "Video", mov: "Video",
  zip: "Archive", rar: "Archive", "7z": "Archive", tar: "Archive",
  exe: "Executable", dll: "DLL", lock: "Lockfile",
};

export function fileTypeLabel(name: string): string {
  // 图先判：svg / html 之类既是文本又是媒体的，靠后会被语言表当成源码类型。
  if (isImageExt(name)) {
    const ext = name.split(".").pop()?.toLowerCase() ?? "";
    return (EXT_TYPE_LABELS[ext] ?? ext.toUpperCase()) || "图片";
  }
  if (isHtmlExt(name)) return "HTML";
  if (isMarkdownExt(name)) return "Markdown";
  const lang = detectLanguage(name);
  if (lang) return LANG_LABELS[lang] ?? lang;
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return (EXT_TYPE_LABELS[ext] ?? ext.toUpperCase()) || "预览";
}

/** Format a timestamp (ms since epoch) as a short date string. */
export function formatModifiedDate(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  const yyyy = d.getFullYear();
  const mm = String(d.getMonth() + 1).padStart(2, "0");
  const dd = String(d.getDate()).padStart(2, "0");
  const hh = String(d.getHours()).padStart(2, "0");
  const mi = String(d.getMinutes()).padStart(2, "0");
  return `${yyyy}/${mm}/${dd} ${hh}:${mi}`;
}

/** Split a path into breadcrumb segments for the address bar.
 *  Handles both `\` (Windows) and `/` (POSIX) separators. */
export function pathToBreadcrumbs(p: string): Array<{ label: string; path: string }> {
  const parts = p.split(/[\\/]/).filter(Boolean);
  const result: Array<{ label: string; path: string }> = [];
  let acc = "";
  for (let i = 0; i < parts.length; i++) {
    if (i === 0 && /^[A-Za-z]:$/.test(parts[0])) {
      // Drive letter like "C:" → "C:\"
      acc = parts[0] + "\\";
      result.push({ label: parts[0], path: acc });
    } else {
      acc = acc ? acc + "\\" + parts[i] : parts[i];
      result.push({ label: parts[i], path: acc });
    }
  }
  return result;
}
