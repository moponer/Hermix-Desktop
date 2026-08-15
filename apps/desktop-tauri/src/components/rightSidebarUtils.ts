// ── Pure helpers for the right sidebar tabs ─────────────
// 纯函数 / 常量：标签管理、格式化、扩展名与语言检测。
// 不依赖 React，可被 ComputerTab / ProductsTab / PreviewPanel 共用。
import type { ElementType } from "react";
import {
  FolderOpen, FileCode, FileText, FileJson, FileImage, FileType2,
} from "lucide-react";
import type { ArtifactKind } from "../utils/artifacts";
import type { PreviewModel } from "../lib/fileParser";

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

/* Image extensions routed through the `image` preview path. */
export const IMAGE_EXT_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  ico: "image/x-icon",
  tiff: "image/tiff",
  tif: "image/tiff",
};
export const IMAGE_BYTE_LIMIT = 8 * 1024 * 1024; // 8 MiB

export function isImageExt(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return ext in IMAGE_EXT_MIME;
}

export function isMarkdownExt(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return ext === "md" || ext === "mdx" || ext === "markdown";
}

export function isPdfExt(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return ext === "pdf";
}

export function detectLanguage(name: string): string | null {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    // 编程语言
    ts: "typescript", tsx: "tsx", js: "javascript", jsx: "jsx",
    mjs: "javascript", cjs: "javascript",
    rs: "rust", py: "python", pyw: "python", go: "go", java: "java",
    kt: "kotlin", kts: "kotlin",
    c: "c", h: "c", cpp: "cpp", cxx: "cpp", hpp: "cpp", cc: "cpp", hh: "cpp",
    cs: "csharp", rb: "ruby", php: "php",
    swift: "swift", lua: "lua", dart: "dart", scala: "scala", sc: "scala",
    ex: "elixir", exs: "elixir", hs: "haskell", nim: "nim", zig: "zig",
    jl: "julia", r: "r", sol: "solidity", proto: "protobuf",
    // Shell / 脚本
    sh: "bash", bash: "bash", zsh: "bash", fish: "bash",
    ps1: "powershell", psm1: "powershell",
    bat: "batch", cmd: "batch",
    // 标记 / 模板
    css: "css", scss: "scss", less: "less",
    html: "markup", htm: "markup", xml: "markup", svg: "markup",
    vue: "markup", svelte: "markup", razor: "markup",
    pug: "pug", jade: "pug", hbs: "handlebars",
    // 数据 / 配置
    json: "json", json5: "json5", jsonp: "jsonp",
    yaml: "yaml", yml: "yaml", toml: "toml",
    ini: "ini", conf: "ini", cfg: "ini", env: "properties",
    properties: "properties", props: "properties",
    graphql: "graphql", gql: "graphql", sql: "sql",
    // 文档
    md: "markdown", mdx: "markdown",
    adoc: "asciidoc", asc: "asciidoc",
    rst: "rest", wiki: "wiki", tex: "latex",
    // 构建 / 工具
    dockerfile: "docker",
    makefile: "makefile", make: "makefile", cmake: "cmake",
    gitignore: "ignore", nginx: "nginx",
    vim: "vim", viml: "vim", vimrc: "vim",
    // 其他
    diff: "diff", patch: "diff", http: "http", regex: "regex",
    csv: "csv", tsv: "csv", mermaid: "mermaid", git: "git",
    txt: "text", ino: "cpp",
  };
  const lang = map[ext] ?? null;
  // Markdown 走专门的 Markdown 预览路径，不走 CodeView
  if (lang === "markdown" || lang === "text") return null;
  return lang;
}

/** File extensions the front-end parses itself. */
export const FRONTEND_PARSE_EXTS = new Set([
  "xlsx", "xls", "xlsm",
  "csv", "tsv",
]);

export function needsFrontendParse(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return FRONTEND_PARSE_EXTS.has(ext);
}

export function totalRows(model: PreviewModel): number {
  if (model.kind === "spreadsheet") return model.sheets.reduce((s, sh) => s + sh.rowCount, 0);
  if (model.kind === "csv") return model.rowCount;
  return 0;
}

export function modelLabel(model: PreviewModel): string {
  if (model.kind === "spreadsheet") return `${model.sheets.length} 个工作表`;
  if (model.kind === "csv") return "CSV";
  return "Text";
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
