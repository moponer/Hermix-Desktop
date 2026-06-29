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

import { Children, isValidElement, useState } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { Prism as SyntaxHighlighter } from "react-syntax-highlighter";
import { oneLight } from "react-syntax-highlighter/dist/esm/styles/prism";
import { Check, Copy } from "lucide-react";

/* ── Path linkification ──────────────────────────────────────── */

// 2026-06-22: switched `\w` to `\p{L}\p{N}` with the `u` flag so
// Chinese filenames (e.g. `D:\data\发票汇总.xlsx`) match. The old
// regex only matched ASCII paths, which is why the user's
// assistant reply "已生成 D:\data\发票汇总.xlsx …" rendered the
// path as plain text instead of a clickable blue underlined link.
// `\p{L}` matches any Unicode letter, `\p{N}` any digit, which
// covers Latin + Han + digits + underscores via \w equivalents,
// minus the underscore which we add back below.
const PATH_RE = /(?:[A-Za-z]:)?(?:[\\/][\p{L}\p{N}_\-.\s()（）【】\[\]]+){1,}\.[\p{L}\p{N}]{1,5}/giu;
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

function CodeBlock({ language, code }: { language: string; code: string }) {
  const [copied, setCopied] = useState(false);
  const handleCopy = async () => {
    await navigator.clipboard.writeText(code);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };
  return (
    // 浅色独立代码块(参考 IDE 截图风格):
    //   - wrapper 浅灰 #fafafa + 1px 标准边框
    //   - header 白 #ffffff "浮"在代码区之上(比 code 亮一档)
    //   - code 区继承 wrapper 浅灰,oneLight 语法高亮
    //   - 浅气泡 + 浅代码块通过 1-2 档色调差形成"独立容器"感
    <div
      className="not-prose my-2.5 overflow-hidden rounded-md"
      style={{
        background: "var(--color-bg-code)",
        border: "1px solid var(--color-bg-code-border)",
      }}
    >
      {/* Header: 比 code 区亮一档(白) + 底边线 subtle 1px */}
      <div
        className="flex items-center justify-between px-4 py-2 text-[11px] border-b"
        style={{
          background: "var(--color-bg-code-header)",
          color: "var(--color-text-faint)",
          borderColor: "var(--color-border-subtle)",
        }}
      >
        <span className="font-mono uppercase tracking-wider">
          {LANG_LABEL[language] ?? language}
        </span>
        <button
          onClick={handleCopy}
          className="flex items-center gap-1.5 rounded px-1.5 py-0.5 transition-colors hover:bg-black/5"
          style={{ color: "var(--color-text-faint)" }}
        >
          {copied ? <Check size={11} /> : <Copy size={11} />}
          <span className="font-medium">{copied ? "已复制" : "复制"}</span>
        </button>
      </div>
      <SyntaxHighlighter
        language={language}
        style={oneLight}
        customStyle={{
          margin: 0,
          borderRadius: 0,
          fontSize: "13px",
          padding: "16px",  // 四边统一 16px inset
          background: "transparent",  // 继承 wrapper 浅灰
          lineHeight: "1.65",
        }}
        PreTag="pre"
        preTagProps={{
          // The highlighter wraps the content in `<pre>` which
          // would otherwise carry its own background. Strip it so
          // the wrapper's surface is the only one in play.
          className: "code-block-body",
          style: { background: "transparent", margin: 0 },
        }}
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
            if (match) return <CodeBlock language={match[1]} code={codeString} />;
            return <code {...props}>{children}</code>;
          },
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
