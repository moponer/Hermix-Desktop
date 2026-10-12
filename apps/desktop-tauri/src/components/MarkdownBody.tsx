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

import { Children, isValidElement, memo, useCallback, useMemo, useRef, useState, useEffect } from "react";
import { markdownToHast } from "satteri";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import type { ReactNode } from "react";
import { Check, Copy, X } from "lucide-react";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { highlightToReact, readIsDark } from "../lib/codeHighlight";
import { autoCloseUnbalancedFences } from "../lib/markdownFence";
import { hardenSoftLineBreaks } from "../lib/softBreaks";
import { FileTypeIcon } from "./FileTypeIcon";

/**
 * 行内代码的语言猜测 —— CommonMark 的 `` `foo` `` 不带语言标记，
 * 但 refractor 必须知道语言才能上色。没有语言就退回纯文本（不猜错
 * 比猜错好：把 shell 当 Rust 高亮出来的颜色是误导）。
 *
 * 判据按"特征最强"排序，每条都要求整串匹配 —— 行内代码通常只有
 * 一个 token 或一个短表达式，不是完整语句。
 */
function guessInlineLang(code: string): string | null {
  const s = code.trim();
  if (!s) return null;
  // 标识符 / 成员访问 / 调用：canonicalPath()、SideTab、foo.bar.baz()
  if (/^[A-Za-z_$][\w$]*(?:\.[A-Za-z_$][\w$]*)*\s*\(\s*\)$/.test(s)) return "javascript";
  if (/^[A-Z][\w$]*(?:\.[A-Z][\w$]*)*$/.test(s)) return "typescript";
  if (/^[a-z_$][\w$]*(?:\.[a-z_$][\w$]*)+$/.test(s)) return "javascript";
  // 路径 / URL —— 交给链接化处理，不高亮（否则斜杠会被当除号上色）
  if (/[\\/]/.test(s) || /^https?:/.test(s)) return null;
  // 字面量：字符串 / 数字 / 布尔
  if (/^(['"]).*\1$/.test(s) || /^-?\d[\d_.]*$/.test(s)) return "javascript";
  if (/^(true|false|null|undefined|NaN)$/.test(s)) return "javascript";
  // CSS 变量 --x / @x / 尺寸单位
  if (/^--[\w-]+$/.test(s) || /^@[\w-]+$/.test(s) || /^[\d.]+(px|rem|em|vh|vw|%)$/.test(s)) {
    return "css";
  }
  return null;
}

/* ── Path linkification ──────────────────────────────────────── */

// 字符类含 ~：Windows 8.3 短路径（ADMINI~1）必须整体命中，否则只匹配到
// ADMINI 就断掉，整条路径识别失败。
//
// 两个分支：带目录分隔符的路径，以及**裸文件名**（`add.py`、`App.tsx`）。
// 后者是 LLM 正文里最常见的写法 —— 需求文档里写"改 App.tsx 的登录逻辑"
// 不带任何目录，原正则要求至少一个 `[\\/]`，这类 token 全部漏掉，
// 结果就是文件图标和可点击预览都没了。
//
// `%VAR%` 前缀（`%TEMP%\x\y.csv`）：Windows 环境变量路径。`%` 不在下面的
// 路径字符类里，但它是 `\p{P}`，能通过 lookbehind —— 如果不显式匹配，
// match 会从 `\x\y.csv` 开始，环境变量前缀被整段切掉，点击探测永远失败。
// 所以在路径分支最前面加一个可选的 `(%VAR%)+` 段。
const PATH_RE = /(?<=^|[\s\p{P}\p{Z}])(?:%[\p{L}\p{N}_]+%(?:[\\/][\p{L}\p{N}_\-~.\s()（）【】\[\]]+)*)?(?:[A-Za-z]:)?(?:[\\/][\p{L}\p{N}_\-~.\s()（）【】\[\]]+){1,}\.[\p{L}\p{N}]{1,5}(?=$|[\s\p{P}\p{Z}])|(?<=^|[\s\p{P}\p{Z}])[\p{L}\p{N}_\-~]+\.[\p{L}\p{N}]{1,5}(?=$|[\s\p{P}\p{Z}])/giu;
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
  // %VAR% 开头（%TEMP%\x\y.csv）：后端会把环境变量展开后再探盘，语义上
  // 和盘符绝对路径一样"一定能定位"，不该染成相对路径的警告琥珀色。
  const env = /^%[\p{L}\p{N}_]+%/u.test(path);
  // 只给**带扩展名**的路径配图标：目录路径（`crates/hermix-agent`）没有
  // 文件类型，FileTypeIcon 会退化成灰色 "?" 问号，反而是噪声。
  const name = path.split(/[\\/]/).pop() ?? path;
  const showIcon = /\.[\p{L}\p{N}]{1,5}$/u.test(name);
  // 裸文件名（`App.tsx`）没有目录上下文，不按"相对路径"处理 ——
  // 染成警告琥珀色是在说"这个路径可能解析不到"，但它本来就是正文里
  // 的中性引用（"改 App.tsx 的登录逻辑"），不是一条待解析的路径。
  const bare = !/[\\/]/.test(path);
  const tone = abs || env ? "abs" : bare ? "bare" : "rel";
  return (
    <button
      type="button"
      onClick={(e) => {
        e.stopPropagation();
        onClick(path, e);
      }}
      title={env ? `环境变量路径：${path}` : abs ? `打开 ${path}` : bare ? name : `相对路径：将以工作区根为基准解析 ${path}`}
      className={`md-path-link md-path-link--${tone}`}
    >
      {/* 13px：正文是中文时，汉字字面占满整个 em box（约 14px 高），
          不是拉丁文的小 cap 高度。CSS 里那套"11px = cap 高度"的实测是按
          拉丁排版量的，对中文正文图标显得过小。13px + 图标自身
          translateY(2px)（见 components-prose.css 的 .md-path-link__icon）
          让图标光学居中于汉字行。 */}
      {showIcon && <FileTypeIcon name={name} size={13} className="md-path-link__icon" />}
      {/* 只显示文件名：正文里写的是一整串 `<repo-parent>\...\db1.py`，铺满整行
          把句子撕成两段，读者真正要找的是 `db1.py`。目录信息不丢 —— title
          属性仍是完整路径，悬停即见。 */}
      <span className="truncate">{name}</span>
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
      if (name === "pre") return child;
      if (name === "code") {
        // 行内 code（无 language- 类）也要 linkify：LLM 爱把文件路径写在
        // 反引号里（`C:\...\x.png`），跳过它 = 结果列表里的路径永远点不了。
        // 块级代码（language-xxx）保持原样，正文代码块不该长出链接。
        const cls = (child.props as { className?: string } | null)?.className ?? "";
        if (cls.includes("language-")) return child;
        const props = child.props as { children?: ReactNode };
        if (props?.children) {
          return { ...child, props: { ...props, children: linkifyChildren(props.children, onPathClick) } };
        }
        return child;
      }
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

const MARKDOWN_REPARSE_THROTTLE_MS = 120;

/**
 * Trailing-edge throttle：高频更新的 value 最多每 intervalMs 提交一次快照，
 * 停止更新后保证最终值落地。流式 chunk 按 rAF 逐帧到达时，全量 Markdown
 * 重解析从每帧一次降到 ~8 次/秒，尾帧定时器保证收尾内容不丢。
 */
function useThrottledValue<T>(value: T, intervalMs: number): T {
  const [snapshot, setSnapshot] = useState(value);
  const lastAtRef = useRef(0);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    const elapsed = Date.now() - lastAtRef.current;
    if (elapsed >= intervalMs) {
      lastAtRef.current = Date.now();
      setSnapshot(value);
      return;
    }
    timerRef.current = setTimeout(() => {
      lastAtRef.current = Date.now();
      setSnapshot(value);
    }, intervalMs - elapsed);
    return () => {
      if (timerRef.current !== null) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
  }, [value, intervalMs]);
  return snapshot;
}

export const MarkdownBody = memo(function MarkdownBody({ content, onPathClick }: MarkdownBodyProps) {
  const handlePathClick = useCallback(onPathClick ?? (() => {}), [onPathClick]);
  const isDark = readIsDark();
  const displayContent = useThrottledValue(content, MARKDOWN_REPARSE_THROTTLE_MS);
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
          // 行内 code：药丸底色 + 复用 refractor 上色（与代码块同一套
          // 引擎与配色，两者不会漂移）。`prop` 是 CommonMark 里的行内代码
          // —— 带反引号的 `canonicalPath()` / `SideTab` 属于这一类。
          const isProp = (props as { prop?: string } | undefined)?.prop === "true";
          return (
            <code {...props} className="md-inline-code">
              {isProp ? highlightToReact(codeString, guessInlineLang(codeString), isDark) : children}
            </code>
          );
        },
        input({ type, ...props }: { type?: string }) {
          if (type === "checkbox") return null;
          return <input type={type} {...props} />;
        },
      },
    });
  }, [displayContent, handlePathClick, isDark]);

  return <div className="prose max-w-none">{rendered}</div>;
});
