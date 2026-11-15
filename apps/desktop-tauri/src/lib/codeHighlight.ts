// Unified code-highlight module. Both the chat `MarkdownBody` and the
// preview `CodeView` route through here; refractor (synchronous,
// already bundled for `CodeView`) is the single highlighter so the
// two surfaces can't drift out of sync. `react-syntax-highlighter`'s
// Prism was tried first — it dynamically imports language chunks at
// runtime, but Tauri's asset protocol doesn't serve those chunks
// inside the packaged webview, so most languages silently fall back
// to plain text. `highlightToReact()` is the only public entry.

import refractor from "refractor";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";

// refractor 3.x ships as CJS (`module.exports = refract`) with named
// exports attached at runtime — Vite/esbuild's static analysis can't
// see them, so `import { highlight, register } from "refractor"` yields
// `undefined`. Use the default import + destructure.
const { highlight, register } = refractor;

// Safe wrapper around `register`. refractor's `register()` skips when
// `grammar.displayName` already exists (a dedup guard). `core.js` calls
// `register(js)` on load — its `displayName` is "javascript" — and
// pins a *lite* javascript grammar at `refract.languages.javascript`
// whose `template-string` is a bare RegExp without `.pattern`. Our
// later `register(javascript)` for the full grammar is then a no-op,
// and `js-templates` / `js-extras` crash when they read `.pattern.source`
// on the bare RegExp, taking the entire highlight module with it.
//
// Two guards:
//   1. `evictLiteJavascript` runs BEFORE the first `register(javascript)`
//      so the full grammar actually installs.
//   2. Each `register` call is wrapped in try/catch — one broken
//      grammar only logs a warning, never breaks global highlighting.
function safeRegister(grammar: Parameters<typeof register>[0]): void {
  try {
    register(grammar);
  } catch (err) {
    if (typeof console !== "undefined") {
      const name =
        grammar && typeof grammar === "object" && "displayName" in grammar
          ? String((grammar as { displayName?: unknown }).displayName)
          : "?";
      console.warn(`[codeHighlight] 语言注册失败 "${name}":`, err);
    }
  }
}

// Evict the lite javascript registered by `core.js` so the full
// grammar below isn't dropped by the dedup guard. `languages` is a
// runtime-only property, not declared in the types — cast through
// `unknown`.
const refractorWithLanguages = refractor as unknown as {
  languages?: Record<string, unknown>;
};
if (
  refractorWithLanguages.languages &&
  refractorWithLanguages.languages.javascript
) {
  delete refractorWithLanguages.languages.javascript;
}

// Core languages — 首屏图里的常驻语法。
//
// 为什么不是全部：这个模块曾经静态引入 66 个语法，`vendor-highlight` 因此
// 604KB，且所有语法在**模块求值**时就注册（一次几十毫秒的长任务），而真实
// 会话里 90% 的代码块是 ts/tsx/js/python/rust/bash/json/yaml（见
// `LANG_LABEL` 与历史会话）。**长尾语法改成按需 import**：第一次遇到才取，
// 取回来注册并通知订阅者重渲染（见 `subscribeLangLoads`）。
import typescript from "refractor/lang/typescript";
import javascript from "refractor/lang/javascript";
import jsxLang from "refractor/lang/jsx";
import tsxLang from "refractor/lang/tsx";
import python from "refractor/lang/python";
import rust from "refractor/lang/rust";
import go from "refractor/lang/go";
import java from "refractor/lang/java";
import c from "refractor/lang/c";
import cpp from "refractor/lang/cpp";
import csharp from "refractor/lang/csharp";
import bash from "refractor/lang/bash";
import powershell from "refractor/lang/powershell";
import css from "refractor/lang/css";
import cssExtras from "refractor/lang/css-extras";
import json from "refractor/lang/json";
import json5 from "refractor/lang/json5";
import yaml from "refractor/lang/yaml";
import toml from "refractor/lang/toml";
import xml from "refractor/lang/markup";
import sql from "refractor/lang/sql";
import diff from "refractor/lang/diff";
import markdown from "refractor/lang/markdown";
import jsExtras from "refractor/lang/js-extras";
import jsTemplates from "refractor/lang/js-templates";

// `safeRegister` swallows individual failures (missing internal
// dependency, broken grammar) so one bad language never blocks the
// rest from installing or breaks the module load.
safeRegister(typescript); safeRegister(javascript);
safeRegister(jsxLang); safeRegister(tsxLang); safeRegister(python);
safeRegister(rust); safeRegister(go); safeRegister(java);
safeRegister(c); safeRegister(cpp); safeRegister(csharp);
safeRegister(bash); safeRegister(powershell);
safeRegister(css); safeRegister(cssExtras);
safeRegister(json); safeRegister(json5); safeRegister(yaml); safeRegister(toml);
safeRegister(xml); safeRegister(sql); safeRegister(diff); safeRegister(markdown);
safeRegister(jsExtras); safeRegister(jsTemplates);

/**
 * 长尾语法的按需加载表（key = refractor 的 canonical 名，与
 * `toRefractorLang` 的输出对齐）。只在真的遇到该语言时才发请求。
 */
const LAZY_LANGS: Record<string, () => Promise<unknown>> = {
  kotlin: () => import("refractor/lang/kotlin"),
  ruby: () => import("refractor/lang/ruby"),
  php: () => import("refractor/lang/php"),
  scss: () => import("refractor/lang/scss"),
  less: () => import("refractor/lang/less"),
  docker: () => import("refractor/lang/docker"),
  swift: () => import("refractor/lang/swift"),
  lua: () => import("refractor/lang/lua"),
  ini: () => import("refractor/lang/ini"),
  batch: () => import("refractor/lang/batch"),
  makefile: () => import("refractor/lang/makefile"),
  cmake: () => import("refractor/lang/cmake"),
  ignore: () => import("refractor/lang/ignore"),
  properties: () => import("refractor/lang/properties"),
  graphql: () => import("refractor/lang/graphql"),
  nginx: () => import("refractor/lang/nginx"),
  http: () => import("refractor/lang/http"),
  git: () => import("refractor/lang/git"),
  regex: () => import("refractor/lang/regex"),
  r: () => import("refractor/lang/r"),
  latex: () => import("refractor/lang/latex"),
  dart: () => import("refractor/lang/dart"),
  scala: () => import("refractor/lang/scala"),
  elixir: () => import("refractor/lang/elixir"),
  haskell: () => import("refractor/lang/haskell"),
  vim: () => import("refractor/lang/vim"),
  zig: () => import("refractor/lang/zig"),
  nim: () => import("refractor/lang/nim"),
  julia: () => import("refractor/lang/julia"),
  solidity: () => import("refractor/lang/solidity"),
  protobuf: () => import("refractor/lang/protobuf"),
  mermaid: () => import("refractor/lang/mermaid"),
  jsonp: () => import("refractor/lang/jsonp"),
  wiki: () => import("refractor/lang/wiki"),
  rest: () => import("refractor/lang/rest"),
  pug: () => import("refractor/lang/pug"),
  handlebars: () => import("refractor/lang/handlebars"),
  asciidoc: () => import("refractor/lang/asciidoc"),
  csv: () => import("refractor/lang/csv"),
};

const langListeners = new Set<() => void>();
let langVersion = 0;
const loadingLangs = new Set<string>();

/** 订阅"某个语法已加载"（CodeBlock 用它触发一次重渲染，把纯文本升级成高亮）。 */
export function subscribeLangLoads(fn: () => void): () => void {
  langListeners.add(fn);
  return () => {
    langListeners.delete(fn);
  };
}

/** 版本号快照：语法加载完成时 +1，配合 `useSyncExternalStore` 使用。 */
export function langLoadVersion(): number {
  return langVersion;
}

function isLangRegistered(name: string): boolean {
  const langs = refractorWithLanguages.languages;
  return !!langs && name in langs;
}

/**
 * 需要时异步取回语法并注册；取回后 `langVersion++` 通知订阅者。
 *
 * 返回 `false` 表示"本次还高亮不了"（正在加载或没有这个语法的加载器），
 * 调用方应当先退回纯文本 —— 下次渲染就会拿到高亮的版本。
 */
function ensureLang(name: string): boolean {
  if (isLangRegistered(name)) return true;
  const loader = LAZY_LANGS[name];
  if (!loader) return false;
  if (!loadingLangs.has(name)) {
    loadingLangs.add(name);
    void loader()
      .then((mod) => {
        // refractor/lang/* 是 CJS：默认导出即语法对象
        const grammar = (mod as { default?: unknown }).default ?? mod;
        safeRegister(grammar as Parameters<typeof register>[0]);
        langVersion += 1;
        langListeners.forEach((fn) => fn());
      })
      .catch(() => {
        /* 语法包缺失/损坏：保持纯文本，下次不再重试同一语言 */
      });
  }
  return false;
}

// Language alias map: LLM-produced tags (e.g. `html`, `vue`, `sh`)
// → the refractor-registered name. Keep in sync with the imports
// above.
export function toRefractorLang(lang: string): string | null {
  const map: Record<string, string> = {
    // Programming languages
    ts: "typescript", tsx: "tsx", js: "javascript", jsx: "jsx",
    mjs: "javascript", cjs: "javascript",
    rs: "rust", py: "python", pyw: "python", go: "go", java: "java",
    kt: "kotlin", kts: "kotlin",
    c: "c", h: "c", cpp: "cpp", cxx: "cpp", hpp: "cpp", cc: "cpp", hh: "cpp",
    cs: "csharp", csharp: "csharp",
    rb: "ruby", ruby: "ruby", php: "php",
    swift: "swift", lua: "lua", dart: "dart",
    scala: "scala", sc: "scala",
    ex: "elixir", exs: "elixir", elixir: "elixir",
    hs: "haskell", haskell: "haskell",
    nim: "nim", zig: "zig", jl: "julia", julia: "julia",
    r: "r", sol: "solidity", solidity: "solidity",
    proto: "protobuf", protobuf: "protobuf",
    // Shell / scripting
    sh: "bash", bash: "bash", zsh: "bash", fish: "bash",
    ps1: "powershell", powershell: "powershell", psm1: "powershell",
    bat: "batch", cmd: "batch", batch: "batch",
    // Markup / templating
    css: "css", scss: "scss", less: "less",
    html: "markup", htm: "markup", xml: "markup", svg: "markup",
    vue: "markup", svelte: "markup", razor: "markup",
    pug: "pug", jade: "pug", hbs: "handlebars", handlebars: "handlebars",
    mustache: "handlebars",
    // Data / config
    json: "json", json5: "json5", jsonp: "jsonp",
    yaml: "yaml", yml: "yaml", toml: "toml",
    ini: "ini", conf: "ini", cfg: "ini", env: "properties",
    properties: "properties", props: "properties",
    graphql: "graphql", gql: "graphql", sql: "sql",
    mysql: "sql", psql: "sql",
    // Documentation
    md: "markdown", mdx: "markdown", markdown: "markdown",
    adoc: "asciidoc", asciidoc: "asciidoc", asc: "asciidoc",
    rst: "rest", rest: "rest", wiki: "wiki",
    tex: "latex", latex: "latex",
    // Build / tooling
    dockerfile: "docker", docker: "docker",
    makefile: "makefile", make: "makefile", cmake: "cmake",
    "cmake.in": "cmake",
    gitignore: "ignore", ignore: "ignore",
    nginx: "nginx", nginxconf: "nginx",
    vim: "vim", viml: "vim", vimrc: "vim",
    // Misc
    diff: "diff", patch: "diff",
    http: "http", regex: "regex",
    csv: "csv", tsv: "csv",
    mermaid: "mermaid", git: "git",
    ino: "cpp",
  };
  return map[lang] ?? null;
}

// Token palettes. Atom One Dark / Light, matching what
// `react-syntax-highlighter`'s `oneDark` / `oneLight` produced so the
// unified refractor path stays visually consistent with the old
// Prism output.
//
// Token 对比度增强策略（关键字/字符串/函数辨识度）:
//   - keyword 加粗 (fontWeight: 600) — 关键字醒目区分于标识符
//   - boolean/number/constant 加粗 — 字面量一眼可见
//   - function 加粗 — 调用点立刻识别
//   - comment 加亮 + italic — 视觉"次要"但不褪色
//   - 提高 string 饱和度 (绿更绿) — 跟 identifier 黄区分更远
const ONE_DARK: Record<string, React.CSSProperties> = {
  comment: { color: "#7c8694", fontStyle: "italic" },
  prolog: { color: "#7c8694", fontStyle: "italic" },
  doctype: { color: "#7c8694", fontStyle: "italic" },
  cdata: { color: "#7c8694", fontStyle: "italic" },
  punctuation: { color: "#abb2bf" },
  property: { color: "#e06c75" },
  tag: { color: "#e45649" },
  boolean: { color: "#d19a66", fontWeight: 600 },
  number: { color: "#d19a66", fontWeight: 600 },
  constant: { color: "#d19a66", fontWeight: 600 },
  symbol: { color: "#d19a66" },
  deleted: { color: "#e06c75" },
  selector: { color: "#98c379" },
  "attr-name": { color: "#d19a66" },
  string: { color: "#98c379" },
  char: { color: "#98c379" },
  builtin: { color: "#e6c07b" },
  inserted: { color: "#98c379" },
  operator: { color: "#56b6c2" },
  entity: { color: "#e06c75" },
  url: { color: "#61afef" },
  atrule: { color: "#c678dd" },
  "attr-value": { color: "#98c379" },
  keyword: { color: "#c678dd", fontWeight: 600 },
  function: { color: "#61afef", fontWeight: 600 },
  "class-name": { color: "#e6c07b" },
  regex: { color: "#56b6c2" },
  important: { color: "#c678dd", fontWeight: 600 },
  variable: { color: "#e06c75" },
  bold: { color: "#abb2bf", fontWeight: "bold" },
  italic: { color: "#abb2bf", fontStyle: "italic" },
};

// Light-mode counterpart to ONE_DARK. Without this, light-theme code
// blocks used dark tokens on a light background — readable but with
// noticeably worse contrast than dark-mode.
// 同 ONE_DARK 增强策略: keyword/function/boolean 加粗,
// comment 调亮 + italic (浅色背景上中灰易消, 用暖灰 +9 步).
const ONE_LIGHT: Record<string, React.CSSProperties> = {
  comment: { color: "#a0a1a7", fontStyle: "italic" },
  prolog: { color: "#a0a1a7", fontStyle: "italic" },
  doctype: { color: "#a0a1a7", fontStyle: "italic" },
  cdata: { color: "#a0a1a7", fontStyle: "italic" },
  punctuation: { color: "#383a42" },
  property: { color: "#e45649" },
  tag: { color: "#e45649" },
  boolean: { color: "#b07d00", fontWeight: 600 },
  number: { color: "#b07d00", fontWeight: 600 },
  constant: { color: "#b07d00", fontWeight: 600 },
  symbol: { color: "#b07d00" },
  deleted: { color: "#e45649" },
  selector: { color: "#50a14f" },
  "attr-name": { color: "#b07d00" },
  string: { color: "#3a9b3a" },
  char: { color: "#3a9b3a" },
  builtin: { color: "#c18401" },
  inserted: { color: "#50a14f" },
  operator: { color: "#0184bc" },
  entity: { color: "#e45649" },
  url: { color: "#0184bc" },
  atrule: { color: "#a626a4" },
  "attr-value": { color: "#3a9b3a" },
  keyword: { color: "#a626a4", fontWeight: 600 },
  function: { color: "#4078f2", fontWeight: 600 },
  "class-name": { color: "#c18401" },
  regex: { color: "#0184bc" },
  important: { color: "#a626a4", fontWeight: 600 },
  variable: { color: "#e056a0" },
  bold: { color: "#383a42", fontWeight: "bold" },
  italic: { color: "#383a42", fontStyle: "italic" },
};

// Looks up the palette entry for a refractor token className. refractor
// emits multi-class strings like "token comment"; we accept any one
// of the listed classes as a match (last one wins, so the more
// specific token kind outranks the generic "token").
//
// `inline` = 行内 code 芯片：底色已经承担了"这是代码"的重音，关键词再
// 加粗就是两个重音互相抢（用户实机反馈：正文里一堆加粗带底色的词，
// 看着像被涂满高亮，读不下去）。行内一律去字重，代码块不受影响 ——
// 那里没有底色，靠字重区分关键字才有效。
function styleForToken(
  className: string,
  isDark: boolean,
  inline: boolean,
): React.CSSProperties | undefined {
  const palette = isDark ? ONE_DARK : ONE_LIGHT;
  const classes = className.split(/\s+/);
  for (const c of classes) {
    const style = palette[c];
    if (style) return inline ? withoutWeight(style) : style;
  }
  const fallback = palette[className];
  return inline ? withoutWeight(fallback) : fallback;
}

/** 去掉字重（保留颜色）。返回新对象 —— 调色板是共享常量，不能就地改。 */
function withoutWeight(
  style: React.CSSProperties | undefined,
): React.CSSProperties | undefined {
  if (!style) return undefined;
  const { fontWeight: _drop, ...rest } = style;
  return rest;
}

// Tokenizes `source` once and returns a React node tree. Measured
// at ~8 ms for 1000 lines of Python. Returns the raw string when
// the language is missing or tokenize fails (so a highlight failure
// degrades to plain text instead of an empty bubble).
export function highlightToReact(
  source: string,
  language: string | null,
  isDark: boolean = true,
  inline: boolean = false,
): React.ReactNode {
  if (!source) return null;
  if (!language) return source;

  const refractorLang = toRefractorLang(language);
  if (!refractorLang) return source;
  // 长尾语法还没取回来（见 LAZY_LANGS）：先出纯文本，加载完成后
  // `subscribeLangLoads` 的订阅者会重渲染，把这一块升级成高亮。
  if (!ensureLang(refractorLang)) return source;

  try {
    const result = highlight(source, refractorLang);
    // refractor 3.x returns an Array<Element | Text>, NOT a HAST
    // Root. `hast-util-to-jsx-runtime` requires `{type:"root", children:…}`
    // — without the wrapper it silently emits null/empty.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const tree = {
      type: "root" as const,
      children: Array.isArray(result) ? result : [result],
    };
    return toJsxRuntime(tree as any, {
      Fragment,
      jsx,
      jsxs,
      components: {
        // Use `jsx()` rather than JSX so this file can stay `.ts`.
        span: (props: React.HTMLAttributes<HTMLSpanElement>) => {
          const { className, style, ...rest } = props;
          const tokenStyle = className ? styleForToken(className, isDark, inline) : undefined;
          return jsx("span", {
            className,
            style: { ...tokenStyle, ...style },
            ...rest,
          } as React.HTMLAttributes<HTMLSpanElement>);
        },
      },
    });
  } catch (err) {
    if (typeof console !== "undefined") {
      console.warn("[codeHighlight] highlight failed:", err);
    }
    return source;
  }
}

// Reads the active theme.
//
// 主题的唯一来源是 `root.dataset.theme`（见 useTheme.ts 的 applyTheme）——
// **`.dark` 类已废除**（globals.css:23-24），`classList.contains("dark")`
// 恒为 false，早期版本因此让暗色主题下的代码块恒用亮色语法配色。
// 判据改为 `colorScheme`（applyTheme 同步设置），与 PreviewPanel 一致。
//
// 注意：这是**非响应式**读取 —— 主题切换时组件不会自动重绘。
// 需要跟随主题重绘的调用方请用 `useIsDarkTheme()`（hooks/useTheme.ts）。
export function readIsDark(): boolean {
  if (typeof document === "undefined") return false;
  return document.documentElement.style.colorScheme === "dark";
}
