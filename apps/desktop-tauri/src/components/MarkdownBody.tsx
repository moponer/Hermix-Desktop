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

import { Children, isValidElement, memo, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneLight, oneDark } from "react-syntax-highlighter/dist/esm/styles/prism";
import { Check, Copy, X } from "lucide-react";
import { useSafeTimeout } from "../hooks/useSafeTimeout";

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
  rs: "Rust", py: "Python", go: "Go", java: "Java", sql: "SQL",
  json: "JSON", yaml: "YAML", yml: "YAML", toml: "TOML", xml: "XML",
  sh: "Shell", bash: "Bash", css: "CSS", html: "HTML", vue: "Vue",
  md: "Markdown", txt: "Text",
};

/** Reads the current dark-mode state from the DOM.
 *  useTheme() applies the `.dark` class to <html> and <body>.
 *  Reading it here (during render) is safe because it's idempotent
 *  and the parent re-renders when the theme changes, which
 *  re-invokes this function with the updated value. */
function readIsDark(): boolean {
  return typeof document !== "undefined" && document.documentElement.classList.contains("dark");
}

// Exported so PreviewPanel can reuse the same code
// block rendering (language label + copy button + syntax highlighting)
// instead of rendering plain <pre><code> elements.
function CodeBlockImpl({ language, code, isDark = false }: { language: string; code: string; isDark?: boolean }) {
  const safeSetTimeout = useSafeTimeout();
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);
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
        <span className="font-mono uppercase tracking-wider">
          {LANG_LABEL[language] ?? "Code"}
        </span>
        <button
          onClick={handleCopy}
          className="flex items-center gap-1.5 rounded px-1.5 py-0.5 transition-colors hover:bg-bg-hover"
          style={{ color: "var(--color-text-muted)" }}
        >
          {copied ? <Check size={11} /> : failed ? <X size={11} /> : <Copy size={11} />}
          <span className="font-medium">{copied ? "已复制" : failed ? "复制失败" : "复制"}</span>
        </button>
      </div>
      <SyntaxHighlighter
        language={language}
        style={isDark ? oneDark : oneLight}
        customStyle={{
          margin: 0,
          borderRadius: 0,
          fontSize: "13px",
          padding: "16px",  // 四边统一 16px inset
          background: "transparent",  // 继承 wrapper 底色
          lineHeight: "1.65",
        }}
        // Removed preTagProps — react-syntax-highlighter
        // passes it through to the DOM <pre> element, causing a React
        // warning "does not recognize the `preTagProps` prop".  The
        // className we needed (code-block-body) is now applied via CSS
        // targeting `pre` inside the wrapper div.
        codeTagProps={{
          // The inner `<code>` element has its own padding from
          // `.prose pre code`. Reset it so the outer `pre`'s
          // uniform 16px inset is the only thing in effect.
          style: { padding: 0, background: "transparent" },
        }}
      >
        {code}
      </SyntaxHighlighter>
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
        remarkPlugins={[remarkGfm]}
        components={{
          p({ children, ...props }) {
            return <p {...props}>{linkifyChildren(children, handlePathClick)}</p>;
          },
          li({ children, ...props }) {
            return <li {...props}>{linkifyChildren(children, handlePathClick)}</li>;
          },
          code({ className, children, ...props }) {
            const match = /language-(\w+)/.exec(className || "");
            const codeString = String(children).replace(/\n$/, "");
            if (match) return <CodeBlock language={match[1]} code={codeString} isDark={isDark} />;
            return <code {...props}>{children}</code>;
          },
          // v3: LLM 输出里的 `- [ ]` / `- [x]` 任务标记
          // 完全不渲染任何 marker。普通无序列表本来就有 disc 圆点，
          // 在文本里再画方框/勾选字符是冗余噪音（用户反馈）。直接
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
