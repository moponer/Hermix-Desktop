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

// Core languages — registered eagerly at module load so `highlight`
// works without further setup.
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
// Extended / niche languages
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

// `safeRegister` swallows individual failures (missing internal
// dependency, broken grammar) so one bad language never blocks the
// rest from installing or breaks the module load.
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
function styleForToken(className: string, isDark: boolean): React.CSSProperties | undefined {
  const palette = isDark ? ONE_DARK : ONE_LIGHT;
  const classes = className.split(/\s+/);
  for (const c of classes) {
    const style = palette[c];
    if (style) return style;
  }
  return palette[className];
}

// Tokenizes `source` once and returns a React node tree. Measured
// at ~8 ms for 1000 lines of Python. Returns the raw string when
// the language is missing or tokenize fails (so a highlight failure
// degrades to plain text instead of an empty bubble).
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

// Reads the active theme. `useTheme()` toggles a `.dark` class on
// `<html>`, so the call is idempotent and re-runs whenever the
// parent re-renders on theme change.
export function readIsDark(): boolean {
  return typeof document !== "undefined" &&
    document.documentElement.classList.contains("dark");
}
