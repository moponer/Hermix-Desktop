/**
 * codeHighlight — 统一代码高亮模块 (2026-07-26)
 * ──────────────────────────────────────────────────────────────────
 *
 * 设计动机（第一性原理）：
 *
 *  之前聊天 (MarkdownBody) 和预览面板 (CodeView) 用了两套不同的高亮引擎：
 *    · 聊天 CodeBlock → react-syntax-highlighter (Prism async build)
 *    · 预览 CodeView  → refractor (同步)
 *
 *  问题：
 *    1. react-syntax-highlighter 的 Prism 是 **异步** 构建——它在运行时
 *       动态 import prismjs 语言组件。在 Tauri 打包的 webview 里，这些
 *       动态 chunk 可能不可用（asset protocol 不 serve .js chunk），
 *       导致 LLM 输出的很多语言代码块 **静默回退为纯文本**，没有高亮。
 *       这就是用户反馈 "代码高亮支持不够，很多类型没有高亮显示" 的根因。
 *    2. 两套引擎 = 两套语言注册 = 不一致。聊天支持的语言和预览不同步。
 *    3. react-syntax-highlighter + prismjs 占 ~500KB 包体积，而 refractor
 *       已经因为 CodeView 被打包进来了，复用它 = 零额外体积。
 *
 *  解决方案：
 *    统一用 refractor（同步、可靠、已在打包中）作为唯一的高亮引擎。
 *    本模块集中管理：
 *      · 语言注册（60+ 种）
 *      · 语言别名映射（LLM 输出 → refractor 名）
 *      · 亮色/暗色 token 配色（Atom One Light / One Dark）
 *      · highlightToReact() — 一次性 tokenize → React 节点树
 *
 *  对抗性审查：
 *    · refractor 同步 tokenize 1000 行 Python ≈ 8ms → 大文件不卡 ✓
 *    · 语言未注册 → try/catch 回退为纯文本，不 crash ✓
 *    · 亮色/暗色切换 → 运行时读取 .dark class，每次 render 重算 ✓
 *    · 包体积 → refractor 已在打包中，零增量 ✓
 */

import refractor from "refractor";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";

// ── 语言注册 ──────────────────────────────────────────────────
// refractor 3.x 是 CJS — module.exports = refract，属性运行时挂载。
// Vite/esbuild 的静态分析检测不到运行时挂载的 named exports，
// 所以 `import { highlight, register } from "refractor"` 会拿到 undefined。
// 用 default import 拿到完整对象后再解构。
const { highlight, register } = refractor;

// ── 注册安全封装 ──────────────────────────────────────────────
// 根因（2026-07-27 修复）：refractor 的 `register()` 在
// `grammar.displayName` 已存在时直接跳过（去重守卫）。而 core.js 在
// 加载时已经 `register(js)` —— 其 displayName 恰好就是 "javascript"，
// 于是把一份 *lite* 版 javascript grammar 挂到了
// `refract.languages.javascript`。我们随后 `register(javascript)`
// （full 版）被当成 no-op，full 版从未安装。lite 版的
// `template-string` 是个裸 RegExp（无 `.pattern` 属性），而
// `js-templates` / `js-extras` 假设它是 `{pattern: regex}` 对象 →
// 读 `.pattern.source` 抛 "Cannot read properties of undefined"，并
// 使整个模块 import 失败 → 全部代码高亮失效。
//
// 防御：1) 注册前先 evict lite 版 javascript，让 full 版真正装进；
// 2) 每个 register 包 try/catch，单个坏 grammar 只告警、绝不拖垮全局。
// 这也符合"任一语言定义可崩都不应让全局高亮挂掉"的对抗式审查要求。
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

// evict refractor 自带的 lite 版 javascript（core.js 注册的 js，
// displayName="javascript"，template-string 是裸 RegExp 无 .pattern）。
// 必须在本文件任何 `register(javascript)` 之前执行，否则 full 版
// 因去重守卫被跳过。
// 注：`languages` 是 refractor 运行时属性，类型未声明，需 cast。
const refractorWithLanguages = refractor as unknown as {
  languages?: Record<string, unknown>;
};
if (
  refractorWithLanguages.languages &&
  refractorWithLanguages.languages.javascript
) {
  delete refractorWithLanguages.languages.javascript;
}

// 核心语言
import python from "refractor/lang/python";
import typescript from "refractor/lang/typescript";
import javascript from "refractor/lang/javascript";
import jsxLang from "refractor/lang/jsx";
import tsxLang from "refractor/lang/tsx";
import rust from "refractor/lang/rust";
import go from "refractor/lang/go";
import java from "refractor/lang/java";
import kotlin from "refractor/lang/kotlin";
import c from "refractor/lang/c";
import cpp from "refractor/lang/cpp";
import csharp from "refractor/lang/csharp";
import ruby from "refractor/lang/ruby";
import php from "refractor/lang/php";
import bash from "refractor/lang/bash";
import css from "refractor/lang/css";
import cssExtras from "refractor/lang/css-extras";
import scss from "refractor/lang/scss";
import less from "refractor/lang/less";
import json from "refractor/lang/json";
import json5 from "refractor/lang/json5";
import yaml from "refractor/lang/yaml";
import toml from "refractor/lang/toml";
import xml from "refractor/lang/markup";
import sql from "refractor/lang/sql";
// 扩展语言
import diff from "refractor/lang/diff";
import docker from "refractor/lang/docker";
import markdown from "refractor/lang/markdown";
import powershell from "refractor/lang/powershell";
import swift from "refractor/lang/swift";
import lua from "refractor/lang/lua";
import ini from "refractor/lang/ini";
import batch from "refractor/lang/batch";
import makefile from "refractor/lang/makefile";
import cmake from "refractor/lang/cmake";
import ignore from "refractor/lang/ignore";
import properties from "refractor/lang/properties";
import graphql from "refractor/lang/graphql";
import nginx from "refractor/lang/nginx";
import http from "refractor/lang/http";
import git from "refractor/lang/git";
import regex from "refractor/lang/regex";
import rLang from "refractor/lang/r";
import latex from "refractor/lang/latex";
import dart from "refractor/lang/dart";
import scala from "refractor/lang/scala";
import elixir from "refractor/lang/elixir";
import haskell from "refractor/lang/haskell";
import vim from "refractor/lang/vim";
import zig from "refractor/lang/zig";
import nim from "refractor/lang/nim";
import julia from "refractor/lang/julia";
import solidity from "refractor/lang/solidity";
import protobuf from "refractor/lang/protobuf";
import mermaid from "refractor/lang/mermaid";
import jsonp from "refractor/lang/jsonp";
import jsExtras from "refractor/lang/js-extras";
import jsTemplates from "refractor/lang/js-templates";
import wiki from "refractor/lang/wiki";
import rest from "refractor/lang/rest";
import pug from "refractor/lang/pug";
import handlebars from "refractor/lang/handlebars";
import asciidoc from "refractor/lang/asciidoc";
import csv from "refractor/lang/csv";

// 一次性注册所有语言 — 模块加载时执行，全局生效
// 用 safeRegister 包裹：单个语言定义若内部抛错（依赖缺失等），
// 只告警不影响其余语言与模块整体加载。
safeRegister(python); safeRegister(typescript); safeRegister(javascript);
safeRegister(jsxLang); safeRegister(tsxLang); safeRegister(rust); safeRegister(go);
safeRegister(java); safeRegister(kotlin); safeRegister(c); safeRegister(cpp);
safeRegister(csharp); safeRegister(ruby); safeRegister(php); safeRegister(bash);
safeRegister(css); safeRegister(cssExtras); safeRegister(scss); safeRegister(less);
safeRegister(json); safeRegister(json5); safeRegister(yaml); safeRegister(toml);
safeRegister(xml); safeRegister(sql);
safeRegister(diff); safeRegister(docker); safeRegister(markdown); safeRegister(powershell);
safeRegister(swift); safeRegister(lua); safeRegister(ini); safeRegister(batch);
safeRegister(makefile); safeRegister(cmake); safeRegister(ignore); safeRegister(properties);
safeRegister(graphql); safeRegister(nginx); safeRegister(http); safeRegister(git);
safeRegister(regex); safeRegister(rLang); safeRegister(latex); safeRegister(dart);
safeRegister(scala); safeRegister(elixir); safeRegister(haskell); safeRegister(vim);
safeRegister(zig); safeRegister(nim); safeRegister(julia); safeRegister(solidity);
safeRegister(protobuf); safeRegister(mermaid); safeRegister(jsonp); safeRegister(jsExtras);
safeRegister(jsTemplates); safeRegister(wiki); safeRegister(rest); safeRegister(pug);
safeRegister(handlebars); safeRegister(asciidoc); safeRegister(csv);

// ── 语言别名映射 ──────────────────────────────────────────────
// LLM 输出的语言标签（如 `html`、`vue`、`sh`）→ refractor 已注册的语言名
export function toRefractorLang(lang: string): string | null {
  const map: Record<string, string> = {
    // 编程语言
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
    // Shell / 脚本
    sh: "bash", bash: "bash", zsh: "bash", fish: "bash",
    ps1: "powershell", powershell: "powershell", psm1: "powershell",
    bat: "batch", cmd: "batch", batch: "batch",
    // 标记 / 模板
    css: "css", scss: "scss", less: "less",
    html: "markup", htm: "markup", xml: "markup", svg: "markup",
    vue: "markup", svelte: "markup", razor: "markup",
    pug: "pug", jade: "pug", hbs: "handlebars", handlebars: "handlebars",
    mustache: "handlebars",
    // 数据 / 配置
    json: "json", json5: "json5", jsonp: "jsonp",
    yaml: "yaml", yml: "yaml", toml: "toml",
    ini: "ini", conf: "ini", cfg: "ini", env: "properties",
    properties: "properties", props: "properties",
    graphql: "graphql", gql: "graphql", sql: "sql",
    mysql: "sql", psql: "sql",
    // 文档
    md: "markdown", mdx: "markdown", markdown: "markdown",
    adoc: "asciidoc", asciidoc: "asciidoc", asc: "asciidoc",
    rst: "rest", rest: "rest", wiki: "wiki",
    tex: "latex", latex: "latex",
    // 构建 / 工具
    dockerfile: "docker", docker: "docker",
    makefile: "makefile", make: "makefile", cmake: "cmake",
    "cmake.in": "cmake",
    gitignore: "ignore", ignore: "ignore",
    nginx: "nginx", nginxconf: "nginx",
    vim: "vim", viml: "vim", vimrc: "vim",
    // 其他
    diff: "diff", patch: "diff",
    http: "http", regex: "regex",
    csv: "csv", tsv: "csv",
    mermaid: "mermaid", git: "git",
    ino: "cpp",
  };
  return map[lang] ?? null;
}

// ── Token 配色：Atom One Dark ──────────────────────────────────
// 暗色主题配色，与 react-syntax-highlighter 的 oneDark 一致。
const ONE_DARK: Record<string, React.CSSProperties> = {
  comment: { color: "#5c6370", fontStyle: "italic" },
  prolog: { color: "#5c6370", fontStyle: "italic" },
  doctype: { color: "#5c6370", fontStyle: "italic" },
  cdata: { color: "#5c6370", fontStyle: "italic" },
  punctuation: { color: "#abb2bf" },
  property: { color: "#e06c75" },
  tag: { color: "#e45649" },
  boolean: { color: "#d19a66" },
  number: { color: "#d19a66" },
  constant: { color: "#d19a66" },
  symbol: { color: "#d19a66" },
  deleted: { color: "#e06c75" },
  selector: { color: "#d19a66" },
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
  keyword: { color: "#c678dd" },
  function: { color: "#61afef" },
  "class-name": { color: "#e6c07b" },
  regex: { color: "#98c379" },
  important: { color: "#c678dd" },
  variable: { color: "#e06c75" },
  bold: { color: "#abb2bf", fontWeight: "bold" },
  italic: { color: "#abb2bf", fontStyle: "italic" },
};

// ── Token 配色：Atom One Light ─────────────────────────────────
// 亮色主题配色，与 react-syntax-highlighter 的 oneLight 一致。
// 之前 CodeView 只有一套 ONE_DARK，在亮色主题下代码区背景是浅色
// 但 token 颜色是暗色配色，导致对比度差、阅读困难。
const ONE_LIGHT: Record<string, React.CSSProperties> = {
  comment: { color: "#a0a1a7", fontStyle: "italic" },
  prolog: { color: "#a0a1a7", fontStyle: "italic" },
  doctype: { color: "#a0a1a7", fontStyle: "italic" },
  cdata: { color: "#a0a1a7", fontStyle: "italic" },
  punctuation: { color: "#383a42" },
  property: { color: "#e45649" },
  tag: { color: "#e45649" },
  boolean: { color: "#98c80d" },
  number: { color: "#98c80d" },
  constant: { color: "#98c80d" },
  symbol: { color: "#98c80d" },
  deleted: { color: "#e45649" },
  selector: { color: "#98c80d" },
  "attr-name": { color: "#98c80d" },
  string: { color: "#50a14f" },
  char: { color: "#50a14f" },
  builtin: { color: "#c18401" },
  inserted: { color: "#50a14f" },
  operator: { color: "#0184bc" },
  entity: { color: "#e45649" },
  url: { color: "#0184bc" },
  atrule: { color: "#a626a4" },
  "attr-value": { color: "#50a14f" },
  keyword: { color: "#a626a4" },
  function: { color: "#4078f2" },
  "class-name": { color: "#c18401" },
  regex: { color: "#50a14f" },
  important: { color: "#a626a4" },
  variable: { color: "#e056a0" },
  bold: { color: "#383a42", fontWeight: "bold" },
  italic: { color: "#383a42", fontStyle: "italic" },
};

/** 根据 token className 查找配色样式 */
function styleForToken(className: string, isDark: boolean): React.CSSProperties | undefined {
  const palette = isDark ? ONE_DARK : ONE_LIGHT;
  // refractor 的 className 是 "token comment" 形式，取最后一个 class
  const classes = className.split(/\s+/);
  for (const c of classes) {
    const style = palette[c];
    if (style) return style;
  }
  return palette[className];
}

/**
 * 一次性 tokenize 全文，返回 React 节点树。
 * 大文件实测：1000 行 Python ≈ 8ms。
 *
 * @param source  源代码文本
 * @param language 语言标识（如 "python"、"html"、"ts"）
 * @param isDark  是否暗色主题（决定使用 ONE_DARK 还是 ONE_LIGHT 配色）
 * @returns React 节点树；语言不支持时返回原始字符串
 */
export function highlightToReact(
  source: string,
  language: string | null,
  isDark: boolean = true,
): React.ReactNode {
  if (!source) return null;
  if (!language) return source;

  const refractorLang = toRefractorLang(language);
  if (!refractorLang) return source;

  try {
    const result = highlight(source, refractorLang);
    // refractor 3.x returns an Array<Element | Text> (indexed by 0,1,2…),
    // NOT a HAST Root node. hast-util-to-jsx-runtime requires a Root
    // ({ type: "root", children: […] }) — without the wrapper it silently
    // returns null/empty.
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
        // refractor 的 span 自带 class="token ..."
        // 用 jsx() 而非 JSX 语法，这样文件可以保持 .ts 扩展名。
        span: (props: React.HTMLAttributes<HTMLSpanElement>) => {
          const { className, style, ...rest } = props;
          const tokenStyle = className ? styleForToken(className, isDark) : undefined;
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

/**
 * 读取当前是否暗色主题。
 * useTheme() 会在 <html> 上加 .dark class。
 * 每次 render 调用是幂等的，父组件在主题变化时会重新 render。
 */
export function readIsDark(): boolean {
  return typeof document !== "undefined" &&
    document.documentElement.classList.contains("dark");
}
