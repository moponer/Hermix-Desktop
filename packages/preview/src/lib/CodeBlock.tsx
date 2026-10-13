// 共享 CodeBlock —— 聊天 MarkdownBody 与预览 MarkdownView 都用它。
// 关键约束：
//   1. 不依赖任何应用层 hook（useSafeTimeout 这种应用态逻辑要在这里
//      重新内联或换写法），保证包是「只读 react + 给定 deps」的纯渲染层。
//   2. 高亮走 lib/codeHighlight：与 CodeView 同一份，语法不会漂。

import { memo, useEffect, useMemo, useRef, useState } from "react";
import { Check, Copy, X } from "lucide-react";
import { highlightToReact } from "./codeHighlight";

const LANG_LABEL: Record<string, string> = {
  ts: "TypeScript", tsx: "TSX", js: "JavaScript", jsx: "JSX",
  mjs: "JavaScript", cjs: "JavaScript",
  rs: "Rust", py: "Python", pyw: "Python", go: "Go", java: "Java",
  kt: "Kotlin", kts: "Kotlin",
  c: "C", h: "C", cpp: "C++", cxx: "C++", hpp: "C++", cc: "C++", hh: "C++",
  cs: "C#", csharp: "C#",
  rb: "Ruby", php: "PHP",
  sh: "Shell", bash: "Bash", zsh: "Shell", fish: "Shell",
  ps1: "PowerShell", powershell: "PowerShell",
  bat: "Batch", cmd: "Batch", batch: "Batch",
  css: "CSS", scss: "SCSS", less: "Less",
  html: "HTML", htm: "HTML", xml: "XML", svg: "SVG",
  vue: "Vue", svelte: "Svelte", razor: "Razor",
  pug: "Pug", jade: "Pug", hbs: "Handlebars",
  json: "JSON", json5: "JSON5", yaml: "YAML", yml: "YAML",
  toml: "TOML", ini: "INI", conf: "INI", cfg: "INI", env: "Env",
  properties: "Properties", props: "Properties",
  graphql: "GraphQL", gql: "GraphQL", sql: "SQL",
  md: "Markdown", mdx: "MDX", markdown: "Markdown",
  adoc: "AsciiDoc", asciidoc: "AsciiDoc", rst: "reST", tex: "LaTeX",
  dockerfile: "Docker", docker: "Docker",
  makefile: "Makefile", make: "Makefile", cmake: "CMake",
  gitignore: ".gitignore", ignore: ".gitignore",
  nginx: "Nginx", vim: "Vim", viml: "Vim",
  diff: "Diff", patch: "Diff", http: "HTTP", regex: "Regex",
  csv: "CSV", tsv: "CSV", mermaid: "Mermaid",
  swift: "Swift", lua: "Lua", dart: "Dart",
  scala: "Scala", sc: "Scala",
  elixir: "Elixir", ex: "Elixir", exs: "Elixir",
  haskell: "Haskell", hs: "Haskell",
  nim: "Nim", zig: "Zig", julia: "Julia", r: "R",
  sol: "Solidity", solidity: "Solidity",
  proto: "Protobuf", protobuf: "Protobuf",
  txt: "Text", text: "Text",
};

function CodeBlockImpl({
  language,
  code,
  isDark = false,
}: {
  language: string;
  code: string;
  isDark?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // 卸载清掉 copy 反馈定时器。包内不引应用的 useSafeTimeout —— 这里
  // 是包最底层，应该自治，不该回头依赖任何上层 hook。
  useEffect(() => {
    return () => {
      if (timerRef.current !== null) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, []);

  const highlighted = useMemo(
    () => highlightToReact(code, language, isDark),
    [code, language, isDark],
  );

  const lineCount = useMemo(() => {
    let n = 1;
    for (let i = 0; i < code.length; i++) {
      if (code.charCodeAt(i) === 10) n++;
    }
    if (code.endsWith("\n")) n--;
    return Math.max(1, n);
  }, [code]);

  const COLLAPSE_THRESHOLD = 30;
  const shouldCollapse = lineCount > COLLAPSE_THRESHOLD;

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setFailed(false);
    } catch {
      setFailed(true);
      setCopied(false);
    }
    if (timerRef.current !== null) clearTimeout(timerRef.current);
    timerRef.current = setTimeout(() => {
      setCopied(false);
      setFailed(false);
      timerRef.current = null;
    }, 1500);
  };
  return (
    <div
      className="not-prose my-2.5 overflow-hidden rounded-md"
      style={{
        background: "var(--color-bg-code)",
        border: "1px solid var(--color-bg-code-border)",
      }}
    >
      <div
        className="flex items-center justify-between px-4 py-2 text-[length:var(--text-meta)] border-b"
        style={{
          background: "var(--color-bg-code-header)",
          color: "var(--color-text-muted)",
          borderColor: "var(--color-border-subtle)",
        }}
      >
        <div className="flex items-center gap-2">
          <span className="font-mono uppercase tracking-wider">
            {LANG_LABEL[language] ?? "Code"}
          </span>
          {shouldCollapse && (
            <span className="text-[length:var(--text-meta)] opacity-50">{lineCount} 行</span>
          )}
        </div>
        <button
          onClick={handleCopy}
          className="flex items-center gap-1.5 rounded px-1.5 py-0.5 transition-colors hover:bg-bg-hover"
          style={{ color: "var(--color-text-muted)" }}
        >
          {copied ? <Check className="icon-sm" /> : failed ? <X className="icon-sm" /> : <Copy className="icon-sm" />}
          <span className="font-medium">{copied ? "已复制" : failed ? "复制失败" : "复制"}</span>
        </button>
      </div>
      <div style={{ position: "relative" }}>
        <pre
          style={{
            margin: 0,
            padding: "16px",
            background: "transparent",
            overflow: shouldCollapse && !expanded ? "hidden" : "auto",
            fontSize: "13px",
            lineHeight: "1.6",
            maxHeight: shouldCollapse && !expanded ? "640px" : undefined,
          }}
        >
          <code
            className={`language-${language}`}
            style={{
              padding: 0,
              background: "transparent",
              fontFamily: "var(--font-mono)",
            }}
          >
            {highlighted}
          </code>
        </pre>
        {shouldCollapse && !expanded && (
          <div
            style={{
              position: "absolute",
              bottom: 0, left: 0, right: 0,
              height: "56px",
              background: "linear-gradient(to bottom, transparent, var(--color-bg-code) 60%)",
              display: "flex", alignItems: "flex-end", justifyContent: "center",
              paddingBottom: "6px",
              pointerEvents: "none",
            }}
          >
            <button
              onClick={() => setExpanded(true)}
              style={{
                background: "var(--color-bg-code-header)",
                border: "1px solid var(--color-border-subtle)",
                borderRadius: "4px",
                padding: "3px 14px",
                fontSize: "11px",
                color: "var(--color-text-muted)",
                cursor: "pointer",
                pointerEvents: "auto",
                fontFamily: "inherit",
              }}
            >
              展开全部（共 {lineCount} 行）
            </button>
          </div>
        )}
        {shouldCollapse && expanded && (
          <div
            style={{
              display: "flex", justifyContent: "center", padding: "4px",
              borderTop: "1px solid var(--color-border-subtle)",
              background: "var(--color-bg-code-header)",
            }}
          >
            <button
              onClick={() => setExpanded(false)}
              style={{
                background: "transparent", border: "none",
                fontSize: "11px", color: "var(--color-text-muted)",
                cursor: "pointer", fontFamily: "inherit",
              }}
            >
              收起
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

export const CodeBlock = memo(
  CodeBlockImpl,
  (a, b) => a.language === b.language && a.code === b.code && a.isDark === b.isDark,
);
