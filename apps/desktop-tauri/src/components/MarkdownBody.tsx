/**
 * MarkdownBody — shared body renderer used by both the streaming
 * bubble (StreamingBubble) and the rendered message bubble
 * (MessageBubble).
 *
 * 历史：react-markdown (unifiedjs) 处理 markdown → hast → React。已迁
 * 至 satteri —— Rust 解析 + hast-util-to-jsx-runtime 渲染，参考
 * src/__spike__/satteri-pipeline.test.tsx 验证。已保形：
 *   - react-markdown 的 `remark-gfm` 对应 satteri features.gfm
 *   - react-markdown 的 `remark-breaks` 由 src/lib/softBreaks.ts
 *     在 markdown 字符串层把软换行 → "  \n"（CommonMark hard break）
 *   - 不平衡 fence 由 src/lib/markdownFence.ts 在 markdownToHast 之前
 *     关闭
 *   - 全部 components 配置（p / li / code / input）原样保留
 */

import { Children, isValidElement, memo, useMemo, useState } from "react";
import { markdownToHast } from "satteri";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import type { ReactNode } from "react";
import { Check, Copy, X } from "lucide-react";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { highlightToReact, readIsDark } from "../lib/codeHighlight";
import { autoCloseUnbalancedFences } from "../lib/markdownFence";
import { hardenSoftLineBreaks } from "../lib/softBreaks";

/* ── Path linkification ──────────────────────────────────────── */

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
  children: ReactNode,
  onPathClick: (path: string, e: React.MouseEvent) => void,
): ReactNode {
  return Children.map(children, (child) => {
    if (typeof child === "string") {
      const parts: ReactNode[] = [];
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
      const props = child.props as { children?: ReactNode };
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

function inferLangFromCode(code: string): string | null {
  const firstLine = code.split("\n")[0]?.trim() ?? "";
  const m = firstLine.match(
    /^\s*(?:\/\/|#|;|--|rem|::)\s*([\w.\/\\-]+\.([A-Za-z0-9]+))$/
  );
  return m ? m[2]!.toLowerCase() : null;
}

function CodeBlockImpl({ language, code, isDark = false }: { language: string; code: string; isDark?: boolean }) {
  const safeSetTimeout = useSafeTimeout();
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
  const [expanded, setExpanded] = useState(false);

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
      safeSetTimeout(() => setCopied(false), 1500);
    } catch {
      setFailed(true);
      setCopied(false);
      safeSetTimeout(() => setFailed(false), 1500);
    }
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
            <span className="text-[11px] opacity-50">{lineCount} 行</span>
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

/* ── Top-level body renderer ──────────────────────────────── */

export interface MarkdownBodyProps {
  content: string;
  /** Path-click handler for linking tool results to the preview
   *  sidebar. Optional — when omitted, path-click is a no-op
   *  (non-main-chat contexts don't have a sidebar to open). */
  onPathClick?: (path: string, e: React.MouseEvent) => void;
}

// satteri features: 对齐 react-markdown 的 remarkPlugins=[remarkGfm, remarkBreaks]
//   gfm:  tables / footnotes / strikethrough / task lists — 等价 remark-gfm
//   softBreaks: 由 src/lib/softBreaks.ts 在字符串层处理
const SATTERI_FEATURES = { features: { gfm: true } } as const;

export function MarkdownBody({ content, onPathClick }: MarkdownBodyProps) {
  const handlePathClick = onPathClick ?? (() => {});
  const isDark = readIsDark();
  // 1. 不平衡 fence 关闭（与 react-markdown 时一致）
  // 2. 软换行 → 硬换行（satleri 不开 remarkBreaks，等价物在 strings 层）
  // 3. satteri 解析 → hast tree
  // 4. hast-util-to-jsx-runtime → React element
  const rendered = useMemo(() => {
    const balanced = autoCloseUnbalancedFences(content);
    const hardened = hardenSoftLineBreaks(balanced);
    const hast = markdownToHast(hardened, SATTERI_FEATURES);
    return toJsxRuntime(hast, {
      Fragment,
      jsx,
      jsxs,
      components: {
        p({ children, ...props }: { children?: ReactNode }) {
          return <p {...props}>{linkifyChildren(children, handlePathClick)}</p>;
        },
        li({ children, ...props }: { children?: ReactNode }) {
          return <li {...props}>{linkifyChildren(children, handlePathClick)}</li>;
        },
        // hast-util-to-jsx-runtime 给 inline code 的 children 已是字符串；
        // 但 fenced block 是单行/多行 + className="language-xxx"。
        // 用 children + className 与原 react-markdown 一致。
        code({ className, children, ...props }: { className?: string; children?: ReactNode }) {
          const match = /language-([\w-]+)/.exec(className || "");
          const codeString = String(children ?? "").replace(/\n$/, "");
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
        input({ type, ...props }: { type?: string }) {
          if (type === "checkbox") return null;
          return <input type={type} {...props} />;
        },
      },
    });
  }, [content, handlePathClick, isDark]);

  return <div className="prose max-w-none">{rendered}</div>;
}
