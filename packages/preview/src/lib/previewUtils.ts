// 预览管线的纯函数帮助（无 IO，无 React）。
//
// 这些原本散落在 desktop 的 rightSidebarUtils.ts 中，被预览层单方向
// 依赖；为让包能在 viewer 重用，把"按扩展名分流"那批纯函数迁到这里。
// rightSidebarUtils 还保留 tab / 格式化等 UI 关注的事，那些仍是
// desktop 应用层关注的事。

/* Image extensions routed through the `image` preview path. */
export const IMAGE_EXT_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  // jfif 就是 JPEG（老相机/扫描仪的另一个后缀名）。
  jfif: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  ico: "image/x-icon",
  tiff: "image/tiff",
  tif: "image/tiff",
  // avif：Chromium 85+ 原生能解。缺这一行时 avif 会落进「文本」分派，
  // 读回来是二进制 → 用户看到「二进制文件，不支持文本预览」的矛盾文案。
  avif: "image/avif",
  // SVG 是文本没错，但它是一张**图**：走 image 路径由 <img> 渲染，既能看到
  // 成品也不会执行脚本（走代码视图反而看不出图形长什么样）。
  svg: "image/svg+xml",
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

/** 有版面要渲染的网页。走 code 视图的只有它自己，切视图即可看源码。 */
export function isHtmlExt(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return ext === "html" || ext === "htm";
}

export function detectLanguage(name: string): string | null {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    ts: "typescript", tsx: "tsx", js: "javascript", jsx: "jsx",
    mjs: "javascript", cjs: "javascript",
    rs: "rust", py: "python", pyw: "python", go: "go", java: "java",
    kt: "kotlin", kts: "kotlin",
    c: "c", h: "c", cpp: "cpp", cxx: "cpp", hpp: "cpp", cc: "cpp", hh: "cpp",
    cs: "csharp", rb: "ruby", php: "php",
    swift: "swift", lua: "lua", dart: "dart", scala: "scala", sc: "scala",
    ex: "elixir", exs: "elixir", hs: "haskell", nim: "nim", zig: "zig",
    jl: "julia", r: "r", sol: "solidity", proto: "protobuf",
    sh: "bash", bash: "bash", zsh: "bash", fish: "bash",
    ps1: "powershell", psm1: "powershell",
    bat: "batch", cmd: "batch",
    css: "css", scss: "scss", less: "less",
    html: "markup", htm: "markup", xml: "markup", svg: "markup",
    vue: "markup", svelte: "markup", razor: "markup",
    pug: "pug", jade: "pug", hbs: "handlebars",
    json: "json", json5: "json5", jsonp: "jsonp",
    yaml: "yaml", yml: "yaml", toml: "toml",
    ini: "ini", conf: "ini", cfg: "ini", env: "properties",
    properties: "properties", props: "properties",
    graphql: "graphql", gql: "graphql", sql: "sql",
    md: "markdown", mdx: "markdown",
    adoc: "asciidoc", asc: "asciidoc",
    rst: "rest", wiki: "wiki", tex: "latex",
    dockerfile: "docker",
    makefile: "makefile", make: "makefile", cmake: "cmake",
    gitignore: "ignore", nginx: "nginx",
    vim: "vim", viml: "vim", vimrc: "vim",
    diff: "diff", patch: "diff", http: "http", regex: "regex",
    csv: "csv", tsv: "csv", mermaid: "mermaid", git: "git",
    txt: "text", ino: "cpp",
  };
  const lang = map[ext] ?? null;
  if (lang === "markdown" || lang === "text") return null;
  return lang;
}

/** File extensions the front-end parses itself. */
export const FRONTEND_PARSE_EXTS = new Set([
  "xlsx", "xls", "xlsm",
  "csv", "tsv",
]);

/**
 * Office 格式按"能否在浏览器里直接渲染"分流。
 * OOXML（docx / pptx）是 ZIP+XML，纯前端引擎可以解析；老二进制格式
 * （doc / ppt 等）结构复杂，只能抽成文本（markitdown）看。
 */
export type OfficeExtGroup = "docx" | "pptx" | "legacy" | null;

const DOCX_EXTS = new Set(["docx", "docm", "dotx", "dotm"]);
const PPTX_EXTS = new Set(["pptx", "pptm", "potx", "potm", "ppsx", "ppsm"]);
const LEGACY_OFFICE_EXTS = new Set([
  "doc", "xls", "ppt", "wps", "et", "dps", "rtf",
  "odt", "ods", "odp", "ott", "ots", "otp",
]);

export function officeExtGroup(name: string): OfficeExtGroup {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (DOCX_EXTS.has(ext)) return "docx";
  if (PPTX_EXTS.has(ext)) return "pptx";
  if (LEGACY_OFFICE_EXTS.has(ext)) return "legacy";
  return null;
}

export function needsFrontendParse(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return FRONTEND_PARSE_EXTS.has(ext);
}
