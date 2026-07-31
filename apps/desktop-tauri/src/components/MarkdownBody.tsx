/**
 * MarkdownBody — shared body renderer used by both the streaming
 * bubble (StreamingBubble) and the rendered message bubble
 * (MessageBubble). The streaming bubble calls the *same* code path
 * for fenced code blocks so that:
 *   - a `python` fence in a streaming reply renders with the
 *     "PYTHON" label + copy button + oneDark highlighting the
 *     moment the closing ``` arrives (not after stream ends);
 *   - there is no visual "jump" when the streaming bubble
 *     transitions to its final rendered form.
 */

import { Children, isValidElement, memo, useMemo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import { Check, Copy, X } from "lucide-react";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { highlightToReact, readIsDark } from "../lib/codeHighlight";

/* ── Path linkification ──────────────────────────────────────── */

// switched `\w` to `\p{L}\p{N}` with the `u` flag so
// Chinese filenames (e.g. `D:\data\发票汇总.xlsx`) match. The old
// regex only matched ASCII paths, which is why the user's
// assistant reply "已生成 D:\data\发票汇总.xlsx …" rendered the
// path as plain text instead of a clickable blue underlined link.
// `\p{L}` matches any Unicode letter, `\p{N}` any digit, which
// covers Latin + Han + digits + underscores via \w equivalents,
// minus the underscore which we add back below.
// added leading/trailing boundary lookarounds so CJK
// prose like "我们/他们.txt" no longer false-matches. A path must now
// sit next to start-of-string / whitespace / punctuation (incl. the
// Chinese 。，、；：etc. covered by \p{P}) on both sides. The documented
// "已生成 D:\data\发票汇总.xlsx" case keeps a space before the path, so
// legit links still match.
const PATH_RE = /(?<=^|[\s\p{P}\p{Z}])(?:[A-Za-z]:)?(?:[\\/][\p{L}\p{N}_\-.\s()（）【】\[\]]+){1,}\.[\p{L}\p{N}]{1,5}(?=$|[\s\p{P}\p{Z}])/giu;
const ABS_PATH_RE = /^(?:[A-Za-z]:[\\/])|(?:[\/])/;

function isAbsolutePath(p: string): boolean {
  return ABS_PATH_RE.test(p);
}

interface PathLinkProps {
  path: string;
  onClick: (path: string, e: React.MouseEvent) => void;
}

function PathLink({ path, onClick }: PathLinkProps) {
  const abs = isAbsolutePath(path);
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onClick(path, e);
      }}
      title={abs ? `打开 ${path}` : `相对路径：将以工作区根为基准解析 ${path}`}
      className={`md-path-link ${abs ? "md-path-link--abs" : "md-path-link--rel"}`}
    >
      <span className="truncate">{path}</span>
    </button>
  );
}

export function linkifyChildren(
  children: React.ReactNode,
  onPathClick: (path: string, e: React.MouseEvent) => void,
): React.ReactNode {
  return Children.map(children, (child) => {
    if (typeof child === "string") {
      const parts: React.ReactNode[] = [];
      let last = 0;
      PATH_RE.lastIndex = 0;
      let m: RegExpExecArray | null;
      let k = 0;
      while ((m = PATH_RE.exec(child)) !== null) {
        if (m.index > last) parts.push(child.slice(last, m.index));
        parts.push(
          <PathLink key={`pl-${k++}-${m.index}`} path={m[0]} onClick={onPathClick} />,
        );
        last = m.index + m[0].length;
      }
      if (last < child.length) parts.push(child.slice(last));
      return parts.length > 0 ? parts : child;
    }
    if (isValidElement(child)) {
      const t = (child.type as { displayName?: string } | string);
      const name = typeof t === "string" ? t : t?.displayName ?? "";
      if (name === "code" || name === "pre") return child;
      const props = child.props as { children?: React.ReactNode };
      if (props?.children) {
        return { ...child, props: { ...props, children: linkifyChildren(props.children, onPathClick) } };
      }
    }
    return child;
  });
}

/* ── Code block with language label + copy button ──────────── */

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

// ── Language alias normalization is now handled by `toRefractorLang()`
//    in `src/lib/codeHighlight.ts`. The LANG_LABEL map below is only
//    used for display labels in the code block header.

// Exported so PreviewPanel can reuse the same code
// block rendering (language label + copy button + syntax highlighting)
// instead of rendering plain <pre><code> elements.

/**
 * 当 LLM 在围栏 ``` 后面漏写语言标签时，尝试从首行文件名注释推断语言。
 * 例如 `// backend.ts` → `ts`，`# ui.tsx` → `tsx`。
 * 这能挽救大量“代码块没高亮、标题显示 CODE”的情况。
 */
function inferLangFromCode(code: string): string | null {
  const firstLine = code.split("\n")[0]?.trim() ?? "";
  const m = firstLine.match(
    /^\s*(?:\/\/|#|;|--|rem|::)\s*([\w.\/\\-]+\.([A-Za-z0-9]+))$/
  );
  return m ? m[2].toLowerCase() : null;
}

function CodeBlockImpl({ language, code, isDark = false }: { language: string; code: string; isDark?: boolean }) {
  const safeSetTimeout = useSafeTimeout();
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  const [expanded, setExpanded] = useState(false);

  // refractor 同步高亮 — 在 Tauri webview 里可靠运行（不依赖
  // 异步 chunk 加载，不像 react-syntax-highlighter 的 Prism async build）。
  // useMemo 缓存，只在 code/language/isDark 变化时重新 tokenize。
  const highlighted = useMemo(
    () => highlightToReact(code, language, isDark),
    [code, language, isDark],
  );

  // 数行数 — 比 split 快 5-10×，大代码块不卡
  const lineCount = useMemo(() => {
    let n = 1;
    for (let i = 0; i < code.length; i++) {
      if (code.charCodeAt(i) === 10) n++;
    }
    if (code.endsWith("\n")) n--;
    return Math.max(1, n);
  }, [code]);

  // 超过阈值的代码块默认折叠，显示前 30 行 + 渐变遮罩 + 展开按钮。
  // 行高 13px × 1.6 = 20.8px → 30 行 + 16px padding ≈ 640px
  const COLLAPSE_THRESHOLD = 30;
  const shouldCollapse = lineCount > COLLAPSE_THRESHOLD;

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      setFailed(false);
      safeSetTimeout(() => setCopied(false), 1500);
    } catch {
      // Non-secure context / denied permission / Tauri webview edge cases.
      setFailed(true);
      setCopied(false);
      safeSetTimeout(() => setFailed(false), 1500);
    }
  };
  return (
    // 代码块容器: 背景跟随主题 (light: #fafafa / dark: #1a1d23)
    // - wrapper 底色 + 1px 标准边框
    // - header 比 code 区亮一档 (light: #fff / dark: #22252b)
    // - code 区继承 wrapper 底色, 语法高亮按主题切换 (oneLight / oneDark)
    <div
      className="not-prose my-2.5 overflow-hidden rounded-md"
      style={{
        background: "var(--color-bg-code)",
        border: "1px solid var(--color-bg-code-border)",
      }}
    >
      {/* Header: 比 code 区亮一档 + 底边线 subtle 1px */}
      <div
        className="flex items-center justify-between px-4 py-2 text-[11px] border-b"
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
            <span className="text-[10px] opacity-50">{lineCount} 行</span>
          )}
        </div>
        <button
          onClick={handleCopy}
          className="flex items-center gap-1.5 rounded px-1.5 py-0.5 transition-colors hover:bg-bg-hover"
          style={{ color: "var(--color-text-muted)" }}
        >
          {copied ? <Check size={11} /> : failed ? <X size={11} /> : <Copy size={11} />}
          <span className="font-medium">{copied ? "已复制" : failed ? "复制失败" : "复制"}</span>
        </button>
      </div>
      {/* 代码主体 — refractor 同步高亮。
          Highlighting must be synchronous: in the Tauri webview,
          dynamically importing prismjs language chunks can silently
          fail and leave code unhighlighted, so every language is
          registered up front via refractor. */}
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
        {/* 折叠状态：底部渐变遮罩 + 展开按钮 */}
        {shouldCollapse && !expanded && (
          <div
            style={{
              position: "absolute",
              bottom: 0,
              left: 0,
              right: 0,
              height: "56px",
              background: "linear-gradient(to bottom, transparent, var(--color-bg-code) 60%)",
              display: "flex",
              alignItems: "flex-end",
              justifyContent: "center",
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
        {/* 展开状态：底部收起按钮 */}
        {shouldCollapse && expanded && (
          <div
            style={{
              display: "flex",
              justifyContent: "center",
              padding: "4px",
              borderTop: "1px solid var(--color-border-subtle)",
              background: "var(--color-bg-code-header)",
            }}
          >
            <button
              onClick={() => setExpanded(false)}
              style={{
                background: "transparent",
                border: "none",
                fontSize: "11px",
                color: "var(--color-text-muted)",
                cursor: "pointer",
                fontFamily: "inherit",
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

// memo so already-rendered code blocks skip re-tokenization
// on every streaming chunk. This is the real per-chunk cost (not bundle
// size — the highlighter is statically imported). Output depends only on
// (language, code, isDark), so the comparator is exact — isDark is
// included so theme switches force a re-render with the new palette.
export const CodeBlock = memo(
  CodeBlockImpl,
  (a, b) => a.language === b.language && a.code === b.code && a.isDark === b.isDark,
);

/* ── Top-level body renderer ──────────────────────────────── */

export interface MarkdownBodyProps {
  content: string;
  /** Path-click handler for linking tool results to the preview
   *  sidebar. Optional — when omitted, path-click is a no-op
   *  (GroupChatView and other non-main-chat contexts don't have
   *  a sidebar to open). */
  onPathClick?: (path: string, e: React.MouseEvent) => void;
}

export function MarkdownBody({ content, onPathClick }: MarkdownBodyProps) {
  const handlePathClick = onPathClick ?? (() => {});
  const isDark = readIsDark();
  return (
    <div className="prose max-w-none">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkBreaks]}
        components={{
          p({ children, ...props }) {
            return <p {...props}>{linkifyChildren(children, handlePathClick)}</p>;
          },
          li({ children, ...props }) {
            return <li {...props}>{linkifyChildren(children, handlePathClick)}</li>;
          },
          code({ className, children, ...props }) {
            const match = /language-([\w-]+)/.exec(className || "");
            const codeString = String(children).replace(/\n$/, "");
            const inferred =
              !match && codeString.includes("\n")
                ? inferLangFromCode(codeString)
                : null;
            const lang = (match?.[1] ?? inferred ?? "").toLowerCase();
            if (lang) {
              return <CodeBlock language={lang} code={codeString} isDark={isDark} />;
            }
            return <code {...props}>{children}</code>;
          },
          // 完全不渲染任何 marker。普通无序列表本来就有 disc 圆点，
          // 返回 null，让 <li> 的默认 disc marker 承担列表语义即可。
          input({ type, ...props }) {
            if (type === "checkbox") return null;
            return <input type={type} {...props} />;
          },
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
