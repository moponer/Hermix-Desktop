/**
 * MarkdownBody — shared body renderer used by both the streaming
 * bubble (StreamingBubble) and the rendered message bubble
 * (MessageBubble).
 *
 * 历史：react-markdown (unifiedjs) 处理 markdown → hast → React。已迁
 * 至 satteri —— Rust 解析 + hast-util-to-jsx-runtime 渲染，参考
 * src/__spike__/satteri-pipeline.test.tsx 验证。已保形：
 *   - react-markdown 的 `remark-gfm` 对应 satteri features.gfm
 *   - react-markdown 的 `remark-breaks` 由 @hermix/preview 的
 *     hardenSoftLineBreaks 在 markdown 字符串层把软换行 → "  \n"（CommonMark hard break）
 *   - 不平衡 fence 由 src/lib/markdownFence.ts 在 markdownToHast 之前
 *     关闭
 *   - 全部 components 配置（p / li / code / input）原样保留
 */

import { Children, cloneElement, isValidElement, memo, useCallback, useMemo, useRef, useState, useEffect, type ReactElement } from "react";
import { markdownToHast } from "satteri";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import type { ReactNode } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Check, Copy, X, ChevronDown, ListOrdered } from "lucide-react";
import katex from "katex";
import "katex/dist/katex.min.css";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { useIsDarkTheme } from "../hooks/useTheme";
import { highlightToReact } from "../lib/codeHighlight";
import { autoCloseUnbalancedFences } from "../lib/markdownFence";
import { hardenSoftLineBreaks } from "@hermix/preview";
import { FileTypeIcon } from "@hermix/preview/FileTypeIcon";
import { COPIED, COPY_FAILED } from "./strings";

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
  /** 是否渲染文件类型图标，由 `MarkdownBody.pathIcons` 统一控制。 */
  icons: boolean;
  onClick: (path: string, e: React.MouseEvent) => void;
}

function PathLink({ path, icons, onClick }: PathLinkProps) {
  const abs = isAbsolutePath(path);
  // %VAR% 开头（%TEMP%\x\y.csv）：后端会把环境变量展开后再探盘，语义上
  // 和盘符绝对路径一样"一定能定位"，不该染成相对路径的警告琥珀色。
  const env = /^%[\p{L}\p{N}_]+%/u.test(path);
  // 只给**带扩展名**的路径配图标：目录路径（`crates/hermix-agent`）没有
  // 文件类型，FileTypeIcon 会退化成灰色 "?" 问号，反而是噪声。
  // 且只在 `icons` 打开时配 —— 思考块里一段能蹦出十几个彩色方块，正文被
  // 切成色块拼贴画，比没有图标更难读。
  const name = path.split(/[\\/]/).pop() ?? path;
  const showIcon = icons && /\.[\p{L}\p{N}]{1,5}$/u.test(name);
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
      {/* 16px：图标是「彩色方块 + 白色扩展名字母」（PDF / TS / W），字母在
          24 viewBox 里只有 7.5–10.5px，按 13px 渲染出来字母约 5.7px ——
          小到只能看见一个色块，认不出是哪种文件。16px 才让字母够辨读。
          正文是中文时汉字字面占满整个 em box（--text-body 14px），不是拉丁文
          的 cap 高度；图标再配 CSS 的 translateY(2px)（见 components-prose.css
          的 .md-path-link__icon）光学居中于汉字行。 */}
      {showIcon && <FileTypeIcon name={name} size={16} className="md-path-link__icon" />}
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
  icons = false,
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
          <PathLink key={`pl-${k++}-${m.index}`} path={m[0]} icons={icons} onClick={onPathClick} />,
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
          return { ...child, props: { ...props, children: linkifyChildren(props.children, onPathClick, icons) } };
        }
        return child;
      }
      const props = child.props as { children?: ReactNode };
      if (props?.children) {
        return { ...child, props: { ...props, children: linkifyChildren(props.children, onPathClick, icons) } };
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
  const [showLines, setShowLines] = useState(false);
  const isDiff = /^(diff|patch)$/i.test(language);

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
    /* 无描边：代码块靠「代码底 vs 头部底」两档底色区分外壳与内容，
       整块再压在纸面上形成第三档。旧版一圈 1px 描边在这里的作用其实是
       补「代码块与正文分界」—— 底色能承担，描边就多余。 */
    <div className="md-codeblock not-prose my-2.5 overflow-hidden rounded-md">
      <div className="md-codeblock__head flex items-center justify-between px-4 py-2 text-[length:var(--text-meta)]">
        <div className="flex items-center gap-2">
          <span className="font-mono">
            {LANG_LABEL[language] ?? "Code"}
          </span>
          {shouldCollapse && (
            <span className="text-[length:var(--text-meta)] opacity-50">{lineCount} 行</span>
          )}
        </div>
        <div className="flex items-center gap-1">
          <button
            type="button"
            onClick={() => setShowLines((v) => !v)}
            title={showLines ? "隐藏行号" : "显示行号"}
            aria-label={showLines ? "隐藏行号" : "显示行号"}
            aria-pressed={showLines}
            className={`grid size-6 place-items-center rounded transition-colors hover:bg-bg-hover ${showLines ? "text-text-primary" : "text-text-muted"}`}
          >
            <ListOrdered className="icon-sm" />
          </button>
          <button
            type="button"
            onClick={handleCopy}
            className="flex items-center gap-1.5 rounded px-1.5 py-0.5 text-text-muted transition-colors hover:bg-bg-hover hover:text-text-primary"
          >
            {copied ? <Check className="icon-sm" /> : failed ? <X className="icon-sm" /> : <Copy className="icon-sm" />}
            <span className="font-medium">{copied ? COPIED : failed ? COPY_FAILED : "复制"}</span>
          </button>
        </div>
      </div>
      <div style={{ position: "relative" }}>
        <div
          style={{
            overflow: shouldCollapse && !expanded ? "hidden" : "auto",
            maxHeight: shouldCollapse && !expanded ? "640px" : undefined,
          }}
        >
          <div style={{ display: "flex", alignItems: "flex-start", minWidth: "100%" }}>
            {/* 行号列独立成列并 sticky 左侧：横向滚动时序号不跟着跑丢。 */}
            {showLines && (
              <pre
                aria-hidden="true"
                style={{
                  margin: 0,
                  padding: "16px 8px 16px 12px",
                  position: "sticky",
                  left: 0,
                  zIndex: 1,
                  textAlign: "right",
                  fontSize: "13px",
                  lineHeight: "1.6",
                  fontFamily: "var(--font-mono)",
                  userSelect: "none",
                  flex: "0 0 auto",
                }}
              >
                {Array.from({ length: lineCount }, (_, i) => i + 1).join("\n")}
              </pre>
            )}
            <pre
              style={{
                margin: 0,
                padding: "16px",
                fontSize: "13px",
                lineHeight: "1.6",
                flex: "1 1 auto",
                minWidth: 0,
              }}
            >
              <code
                className={`language-${language}`}
                style={{
                  padding: 0,
                  fontFamily: "var(--font-mono)",
                }}
              >
                {isDiff
                  ? code.split("\n").map((line, i) => {
                      const kind =
                        line.startsWith("+++") || line.startsWith("---")
                          ? "meta"
                          : line.startsWith("@@")
                            ? "hunk"
                            : line.startsWith("+")
                              ? "add"
                              : line.startsWith("-")
                                ? "del"
                                : "ctx";
                      return (
                        <span key={i} className={`md-diff-line md-diff-line--${kind}`}>
                          {line || " "}
                          {"\n"}
                        </span>
                      );
                    })
                  : highlighted}
              </code>
            </pre>
          </div>
        </div>
        {/* 折叠遮罩：渐变必须停在代码块底色上（code-fade 定义在 flat-ui.css），
            所以它天然依赖 --color-bg-code —— 用类把它固定在一处。 */}
        {shouldCollapse && !expanded && (
          <div className="code-fade pointer-events-none absolute inset-x-0 bottom-0 flex h-14 items-end justify-center pb-1.5">
            <button
              type="button"
              onClick={() => setExpanded(true)}
              style={{
                borderRadius: "4px",
                padding: "3px 14px",
                fontSize: "var(--text-meta)",
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
            }}
          >
            <button
              type="button"
              onClick={() => setExpanded(false)}
              className="cursor-pointer bg-transparent font-[inherit] text-[length:var(--text-meta)] text-text-muted"
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

/* ── 小节包装：复制本节 / 折叠 ─────────────────────────────────── */

/** 递归取纯文本（复制与标题识别用）。 */
function nodeText(node: ReactNode): string {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(nodeText).join("");
  if (isValidElement(node)) return nodeText((node.props as { children?: ReactNode }).children);
  return "";
}

/** 从节点树头部剥掉 n 个字符（用于去掉引用块里的 [!NOTE] 标记）。 */
function stripLeadingText(node: ReactNode, n: number): ReactNode {
  if (n <= 0) return node;
  if (typeof node === "string") return node.slice(n);
  if (Array.isArray(node)) {
    const out: ReactNode[] = [];
    let left = n;
    for (const child of node) {
      const len = nodeText(child).length;
      if (left >= len) { left -= len; continue; }
      out.push(stripLeadingText(child, left));
      left = 0;
    }
    return out;
  }
  if (isValidElement(node)) {
    const props = node.props as { children?: ReactNode };
    return cloneElement(
      node as ReactElement<{ children?: ReactNode }>,
      { children: stripLeadingText(props.children, n) },
    );
  }
  return node;
}

/* ── 数学公式 ────────────────────────────────────────────────
 * satteri 不认 $...$，这里在解析前把公式换成一个哨兵包裹的行内代码，
 * 由 code 渲染器接住交给 KaTeX。块公式换成 ```math 围栏。
 * 不引 remark-math：多一个 AST 插件，而且我们只需要两种形态。 */
const MATH_SENTINEL = "\uE000";

function replaceInlineMath(line: string): string {
  return line.replace(
    /(^|[^\\$])\$(?!\$)([^\n$]+?)\$(?!\$)/g,
    (m, pre: string, tex: string) => {
      // 只认"像公式"的内容：至少有一个字母/反斜杠/上下标/花括号/等号，
      // 纯数字（$5$）更像金额，不转。含空格又没有任何公式符号的（"$5 and "）
      // 也放过 —— 那是金额区间，不是公式。
      if (!/[A-Za-z\\^_{}=]/.test(tex)) return m;
      if (/\s/.test(tex.trim()) && !/[\\^_{}=]/.test(tex)) return m;
      return `${pre}\`${MATH_SENTINEL}${tex}${MATH_SENTINEL}\``;
    },
  );
}

function transformMath(md: string): string {
  const lines = md.split("\n");
  const out: string[] = [];
  let inFence = false;
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i]!;
    if (/^\s*(```|~~~)/.test(line)) {
      inFence = !inFence;
      out.push(line);
      continue;
    }
    if (inFence) {
      out.push(line);
      continue;
    }
    const open = /^\s*\$\$(.*)$/.exec(line);
    if (open) {
      const oneLine = /^(.*?)\$\$\s*$/.exec(open[1] ?? "");
      if (oneLine) {
        out.push("```math", (oneLine[1] ?? "").trim(), "```");
        continue;
      }
      const body: string[] = [open[1] ?? ""];
      let j = i + 1;
      for (; j < lines.length; j++) {
        const end = /^(.*?)\$\$\s*$/.exec(lines[j]!);
        if (end) {
          body.push(end[1] ?? "");
          break;
        }
        body.push(lines[j]!);
      }
      if (j < lines.length) {
        out.push("```math", body.join("\n").trim(), "```");
        i = j;
        continue;
      }
    }
    out.push(replaceInlineMath(line));
  }
  return out.join("\n");
}

function MathNode({ tex, display }: { tex: string; display?: boolean }) {
  const html = useMemo(() => {
    try {
      return katex.renderToString(tex, {
        displayMode: display === true,
        throwOnError: false,
        strict: false,
        output: "html",
      });
    } catch {
      return null;
    }
  }, [tex, display]);
  if (!html) return <code>{tex}</code>;
  return (
    <span
      className={display ? "md-math md-math--block" : "md-math"}
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

/* ── Mermaid：按需加载（进独立 chunk，主包不背这 1MB） ────────── */
let mermaidPromise: Promise<typeof import("mermaid")> | null = null;
function loadMermaid() {
  if (!mermaidPromise) mermaidPromise = import("mermaid");
  return mermaidPromise;
}

function MermaidBlock({ code, isDark }: { code: string; isDark: boolean }) {
  const [svg, setSvg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showSource, setShowSource] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setSvg(null);
    setError(null);
    loadMermaid()
      .then(async (mod) => {
        const mermaid = mod.default;
        mermaid.initialize({
          startOnLoad: false,
          theme: isDark ? "dark" : "default",
          securityLevel: "strict",
          fontFamily: "var(--font-sans)",
        });
        const id = `hermix-mmd-${Math.random().toString(36).slice(2, 10)}`;
        const out = await mermaid.render(id, code);
        if (!cancelled) setSvg(out.svg);
      })
      .catch((e: unknown) => {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      });
    return () => { cancelled = true; };
  }, [code, isDark]);

  return (
    <div className="md-mermaid not-prose">
      <div className="md-mermaid-head">
        <span className="md-mermaid-label">Mermaid</span>
        <button
          type="button"
          className="md-mermaid-btn"
          onClick={() => setShowSource((v) => !v)}
          aria-pressed={showSource}
        >
          {showSource ? "看图表" : "看源码"}
        </button>
      </div>
      {error ? (
        <pre className="md-mermaid-error">{code}</pre>
      ) : showSource ? (
        <pre className="md-mermaid-source">{code}</pre>
      ) : svg ? (
        <div className="md-mermaid-svg" dangerouslySetInnerHTML={{ __html: svg }} />
      ) : (
        <div className="md-mermaid-loading">正在绘制图表…</div>
      )}
    </div>
  );
}

const CALLOUT_RE = /^\s*\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]\s*/i;
const CALLOUT_LABEL: Record<string, string> = {
  NOTE: "提示", TIP: "技巧", IMPORTANT: "重要", WARNING: "注意", CAUTION: "警告",
};
const CALLOUT_TONE: Record<string, string> = {
  NOTE: "", TIP: "md-callout--success", IMPORTANT: "", WARNING: "md-callout--warning", CAUTION: "md-callout--danger",
};

/** 把 > [!NOTE] 这种块引用识别成提示块，并剥掉标记本身。 */
function extractCallout(children: ReactNode): { kind: string | null; children: ReactNode } {
  const arr = Children.toArray(children);
  const first = arr[0];
  if (!isValidElement(first)) return { kind: null, children };
  const m = CALLOUT_RE.exec(nodeText(first));
  if (!m) return { kind: null, children };
  return {
    kind: (m[1] ?? "").toUpperCase(),
    children: [stripLeadingText(first, m[0].length), ...arr.slice(1)],
  };
}

interface MarkdownSectionModel {
  id: string;
  heading: ReactNode | null;
  body: ReactNode[];
}

/** 按 h1/h2 切小节。h3 留在正文里，避免层级切得太碎。 */
function groupSections(nodes: ReactNode[]): MarkdownSectionModel[] {
  const groups: MarkdownSectionModel[] = [];
  let current: MarkdownSectionModel = { id: "s0", heading: null, body: [] };
  let n = 0;
  for (const node of nodes) {
    const tag = isValidElement(node) ? String(node.type) : "";
    if (tag === "h1" || tag === "h2") {
      if (current.heading || current.body.length) groups.push(current);
      n += 1;
      current = { id: `s${n}-${nodeText(node).slice(0, 24)}`, heading: node, body: [] };
    } else {
      current.body.push(node);
    }
  }
  if (current.heading || current.body.length) groups.push(current);
  return groups;
}

function MarkdownSection({ heading, children }: { heading: ReactNode; children: ReactNode }) {
  const [open, setOpen] = useState(true);
  const [copied, setCopied] = useState(false);
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const safeSetTimeout = useSafeTimeout();

  const copy = async () => {
    const text = `${nodeText(heading)}\n\n${bodyRef.current?.textContent ?? ""}`.trim();
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      safeSetTimeout(() => setCopied(false), 1400);
    } catch {
      /* 剪贴板不可用：保持静默，不打断阅读 */
    }
  };

  return (
    <section className="md-section">
      <div className="md-section-head">
        {heading}
        <div className="md-section-actions">
          <button
            type="button"
            className="md-section-btn"
            onClick={() => void copy()}
            title="复制本节"
            aria-label="复制本节"
          >
            {copied ? <Check className="icon-sm" /> : <Copy className="icon-sm" />}
          </button>
          <button
            type="button"
            className="md-section-btn"
            onClick={() => setOpen((v) => !v)}
            title={open ? "折叠本节" : "展开本节"}
            aria-label={open ? "折叠本节" : "展开本节"}
            aria-expanded={open}
          >
            <ChevronDown className={`icon-sm transition-transform ${open ? "" : "-rotate-90"}`} />
          </button>
        </div>
      </div>
      <div ref={bodyRef} hidden={!open}>{children}</div>
    </section>
  );
}

export interface MarkdownBodyProps {
  content: string;
  /** Path-click handler for linking tool results to the preview
   *  sidebar. Optional — when omitted, path-click is a no-op
   *  (non-main-chat contexts don't have a sidebar to open). */
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /**
   * 正文里的文件路径是否带文件类型图标。**默认关** ——
   *
   * MarkdownBody 不只渲染最终答复：思考块、工具结果正文、更新日志、技能
   * 文档都复用它。那些位置一段话里能有十几个路径，每个前面顶一个彩色
   * 方块，正文被切成色块拼贴画，可读性反而不如纯文本。只有最终答复正文
   * （`ToolCallBubble` 的 `kind === "text"` 步）显式打开。
   */
  pathIcons?: boolean;
  /**
   * 小节交互（复制本节 / 折叠）。**流式渲染传 false**：每个 chunk 重组小节树
   * 会让展开态反复重建；正文定型后再挂交互。
   */
  interactive?: boolean;
}

// satteri features: 对齐 react-markdown 的 remarkPlugins=[remarkGfm, remarkBreaks]
//   gfm:  tables / footnotes / strikethrough / task lists — 等价 remark-gfm
//   softBreaks: 由 @hermix/preview 的 hardenSoftLineBreaks 在字符串层处理
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

/**
 * 归一化 markdown 图片目的地里的 Windows 绝对路径（反斜杠 → 正斜杠）。
 *
 * 必须在 satteri 解析**之前**做：CommonMark 把 `\U` `\.` 等反斜杠当转义/
 * 字面量处理，satteri 会把 `<USERPROFILE>\.hermix\x.png` 搞成
 * `C:%5CUsers%5CAdministrator.hermix%5C...`——反斜杠被编码、`\.` 的反斜杠
 * 被吞，路径信息不可逆丢失，下游 convertFileSrc 无力回天（2026-10-17
 * 对话内截图破图根因）。跳过 fence 内的行，避免改写代码示例。
 */
function normalizeWindowsImagePaths(md: string): string {
  const lines = md.split("\n");
  let inFence = false;
  const out = lines.map((line) => {
    if (/^\s*(```|~~~)/.test(line)) inFence = !inFence;
    if (inFence) return line;
    return line.replace(/(!\[[^\]]*\]\()([^)\s]+)(\))/g, (m, pre: string, dest: string, post: string) => {
      if (!/^[a-zA-Z]:[\\/]|^\\\\/.test(dest)) return m;
      return pre + dest.replace(/\\/g, "/") + post;
    });
  });
  return out.join("\n");
}

export const MarkdownBody = memo(function MarkdownBody({
  content, onPathClick, pathIcons = false, interactive = true,
}: MarkdownBodyProps) {
  const handlePathClick = useCallback(onPathClick ?? (() => {}), [onPathClick]);
  // 主题判定走 useIsDarkTheme（响应式 + 不读已废除的 `.dark` 类）。
  // 早期版本用 readIsDark() 读 `.dark` 类，恒为 false —— 暗色主题下
  // 聊天代码块恒用亮色语法配色（punctuation 在 #1a1a1a 上仅 1.53:1）。
  const isDark = useIsDarkTheme();
  const displayContent = useThrottledValue(content, MARKDOWN_REPARSE_THROTTLE_MS);
  // 1. 不平衡 fence 关闭（与 react-markdown 时一致）
  // 2. 软换行 → 硬换行（satleri 不开 remarkBreaks，等价物在 strings 层）
  // 3. satteri 解析 → hast tree
  // 4. hast-util-to-jsx-runtime → React element
  const rendered = useMemo(() => {
    const normalized = normalizeWindowsImagePaths(content);
    const balanced = autoCloseUnbalancedFences(normalized);
    const hardened = hardenSoftLineBreaks(balanced);
    const withMath = transformMath(hardened);
    const hast = markdownToHast(withMath, SATTERI_FEATURES);
    return toJsxRuntime(hast, {
      Fragment,
      jsx,
      jsxs,
      components: {
        p({ children, ...props }: { children?: ReactNode }) {
          return <p {...props}>{linkifyChildren(children, handlePathClick, pathIcons)}</p>;
        },
        li({ children, className, ...props }: { children?: ReactNode; className?: string }) {
          // GFM task list：勾选框此前被 input 渲染器整个丢掉，列表退化成圆点。
          const kids = Children.toArray(children);
          let checked: boolean | null = null;
          const rest: ReactNode[] = [];
          for (const kid of kids) {
            if (isValidElement(kid) && kid.type === "input") {
              const p = kid.props as { type?: string; checked?: boolean };
              if (p.type === "checkbox") { checked = !!p.checked; continue; }
            }
            rest.push(kid);
          }
          const isTask = checked !== null || /task-list-item/.test(className ?? "");
          return (
            <li {...props} className={[className, isTask ? "md-task" : ""].filter(Boolean).join(" ") || undefined}>
              {isTask && (
                <span className="md-task-box" data-checked={checked ? "true" : "false"} aria-hidden="true">
                  {checked ? "✓" : ""}
                </span>
              )}
              {linkifyChildren(rest, handlePathClick, pathIcons)}
            </li>
          );
        },
        blockquote({ children, ...props }: { children?: ReactNode }) {
          const { kind, children: cleaned } = extractCallout(children);
          if (kind) {
            return (
              <div className={`md-callout ${CALLOUT_TONE[kind] ?? ""}`.trim()} {...props}>
                <span className="md-callout-icon" aria-hidden="true">
                  {kind === "WARNING" || kind === "CAUTION" ? "!" : kind === "TIP" ? "★" : "i"}
                </span>
                <div className="md-callout-body">
                  <span className="md-callout-title">{CALLOUT_LABEL[kind] ?? kind}</span>
                  {linkifyChildren(cleaned, handlePathClick, pathIcons)}
                </div>
              </div>
            );
          }
          return <blockquote {...props}>{linkifyChildren(children, handlePathClick, pathIcons)}</blockquote>;
        },
        // 表格外层加横向滚动容器：宽表不再把列挤成一条线。
        table({ children, ...props }: { children?: ReactNode }) {
          return (
            <div className="md-table-wrap">
              <table {...props}>{children}</table>
            </div>
          );
        },
        th({ children, align, ...props }: { children?: ReactNode; align?: string }) {
          return <th {...props} data-align={align ?? undefined}>{children}</th>;
        },
        td({ children, align, ...props }: { children?: ReactNode; align?: string }) {
          return <td {...props} data-align={align ?? undefined}>{children}</td>;
        },
        // hast-util-to-jsx-runtime 给 inline code 的 children 已是字符串；
        // 但 fenced block 是单行/多行 + className="language-xxx"。
        // 用 children + className 与原 react-markdown 一致。
        code({ className, children, ...props }: { className?: string; children?: ReactNode }) {
          const match = /language-([\w-]+)/.exec(className || "");
          const codeString = String(children ?? "").replace(/\n$/, "");
          // 公式：行内（哨兵包裹）与块（```math）
          if (codeString.length > 2 && codeString.startsWith(MATH_SENTINEL) && codeString.endsWith(MATH_SENTINEL)) {
            return <MathNode tex={codeString.slice(1, -1)} />;
          }
          if ((match?.[1] ?? "").toLowerCase() === "math") {
            return <MathNode tex={codeString} display />;
          }
          const inferred =
            !match && codeString.includes("\n")
              ? inferLangFromCode(codeString)
              : null;
          const lang = (match?.[1] ?? inferred ?? "").toLowerCase();
          if (lang === "mermaid") {
            return <MermaidBlock code={codeString} isDark={isDark} />;
          }
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
        img({ src, alt, ...props }: { src?: string; alt?: string }) {
          // 模型经常把工具落盘的截图（如 browser_screenshot 的
          // `~/.hermix/screenshots/...png`）写成 markdown 图片。WebView2
          // 页面 origin 加载不了磁盘绝对路径，必须经 asset 协议转换——
          // scope 在 tauri.conf.json（`$HOME/.hermix/**`，生产数据根即此）。
          const raw = typeof src === "string" ? src : "";
          const isLocal =
            /^(?:[a-zA-Z]:[\\/]|\\\\|file:\/\/)/i.test(raw) &&
            !/^(?:https?|data|blob|asset|https\+asset):/i.test(raw);
          const resolved = isLocal ? convertFileSrc(raw) : raw;
          return (
            <img
              {...props}
              src={resolved}
              alt={alt ?? ""}
              loading="lazy"
              draggable={false}
              className="my-1 max-w-full rounded-md border border-border-primary/40"
            />
          );
        },
      },
    });
  }, [displayContent, handlePathClick, pathIcons, isDark]);

  // 正文定型后才切小节（复制/折叠）。流式阶段保持扁平，避免每 chunk 重建小节树。
  const body = useMemo(() => {
    if (!interactive) return rendered;
    return groupSections(Children.toArray(rendered)).map((g) =>
      g.heading ? (
        <MarkdownSection key={g.id} heading={g.heading}>{g.body}</MarkdownSection>
      ) : (
        <Fragment key={g.id}>{g.body}</Fragment>
      ),
    );
  }, [rendered, interactive]);

  return <div className="prose max-w-none">{body}</div>;
});
