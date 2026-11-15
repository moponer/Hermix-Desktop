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

import { Children, cloneElement, isValidElement, memo, useCallback, useDeferredValue, useMemo, useRef, useState, useEffect, useSyncExternalStore, type ReactElement } from "react";
import { markdownToHast } from "satteri";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import type { ReactNode } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { Check, Copy, X, ChevronDown, ListOrdered } from "lucide-react";
/* katex 本体按需 import（见 MathNode）：这里只留样式。 */
import "katex/dist/katex.min.css";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { useIsDarkTheme } from "../hooks/useTheme";
import { highlightToReact, langLoadVersion, subscribeLangLoads } from "../lib/codeHighlight";
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

/**
 * 行内路径链接。
 *
 * 图标走 `FileTypeIcon` 的 **soft 形态**（同色相淡染底 + 文字色字形）：
 * 原来的品牌实色方块 + 白色粗体字母在段落里就是一块块彩色积木 —— 用户
 * 实机反馈"一部分重点加粗带背景色的特殊词汇……界面有点乱"。淡染后色相
 * 还在（扫一眼知道是什么类型），重量没了。
 */
function PathLink({ path, icons, onClick }: PathLinkProps) {
  const abs = isAbsolutePath(path);
  // %VAR% 开头（%TEMP%\x\y.csv）：后端会把环境变量展开后再探盘，语义上
  // 和盘符绝对路径一样"一定能定位"，不该染成相对路径的警告琥珀色。
  const env = /^%[\p{L}\p{N}_]+%/u.test(path);
  // 只给**带扩展名**的路径配图标：目录路径（`crates/hermix-agent`）没有
  // 文件类型，FileTypeIcon 会退化成灰色 "?" 问号，反而是噪声。
  // 且只在 `icons` 打开时配 —— 思考块里一段能蹦出十几个方块，正文被
  // 切成拼贴画，比没有图标更难读。
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
      {/* 16px：图标是「色块 + 扩展名字母」（PDF / TS / W），字母在 24 viewBox
          里只有 7.5–10.5px，按 13px 渲染出来字母约 5.7px —— 小到只能看见
          一个色块，认不出是哪种文件。16px 才让字母够辨读。
          正文是中文时汉字字面占满整个 em box（--text-body 14px），不是拉丁文
          的 cap 高度；图标再配 CSS 的 translateY(2px)（见 components-prose.css
          的 .md-path-link__icon）光学居中于汉字行。 */}
      {showIcon && (
        <FileTypeIcon name={name} size={16} variant="soft" className="md-path-link__icon" />
      )}
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
  /** 同一条消息内已带过图标的路径集合：同一文件名反复出现时只有首个
   *  带文件类型图标（见 MarkdownBody 渲染器里 iconSeen 的注记）。
   *  不传时作用域 = 本次调用，调用方要跨段落去重就自己建一个传进来。 */
  iconSeen: Set<string> = new Set<string>(),
): ReactNode {
  // 图标是行内最大的重音件（16px vs 正文 14px），LLM 爱在一段里反复提
  // 同一文件 —— 每次都顶一个图标就是把句子切成拼贴画。重复出现退化为
  // 纯链接，信息不丢（可点、有 title），重量只付一次。
  const claimIcon = (path: string): boolean => {
    if (!icons) return false;
    const first = !iconSeen.has(path);
    iconSeen.add(path);
    return first;
  };
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
          <PathLink key={`pl-${k++}-${m.index}`} path={m[0]} icons={claimIcon(m[0])} onClick={onPathClick} />,
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
        // 整枚行内代码恰是一个路径 → 不套芯片，直接渲染成 PathLink。此前
        // 是「药丸底里再嵌下划线链接」的叠加态：同一个文件名，LLM 写不写
        // 反引号会长成两种样子 —— 正文文件名形态混乱的直接来源之一。
        const whole = nodeText(props?.children);
        if (whole) {
          PATH_RE.lastIndex = 0;
          const m0 = PATH_RE.exec(whole);
          if (m0 && m0.index === 0 && m0[0] === whole) {
            return <PathLink path={whole} icons={claimIcon(whole)} onClick={onPathClick} />;
          }
        }
        if (props?.children) {
          return { ...child, props: { ...props, children: linkifyChildren(props.children, onPathClick, icons, iconSeen) } };
        }
        return child;
      }
      const props = child.props as { children?: ReactNode };
      if (props?.children) {
        return { ...child, props: { ...props, children: linkifyChildren(props.children, onPathClick, icons, iconSeen) } };
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

  // 长尾语法按需加载（见 codeHighlight 的 LAZY_LANGS）：首次遇到时先出纯文本，
  // 语法包到位后版本号 +1 → 这里重算一次，把这块升级成高亮。
  const langVersion = useSyncExternalStore(subscribeLangLoads, langLoadVersion, langLoadVersion);
  const highlighted = useMemo(
    () => highlightToReact(code, language, isDark),
    [code, language, isDark, langVersion],
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

/* ── KaTeX：按需加载（512KB，只有真的出现公式时才取）─────────
   它与 Mermaid 同一形态：静态 import 会让每个打开对话的人都白背一份
   katex 的解析成本，而实际用到公式的回合很少。取不到/解析失败一律退回
   原文 `<code>`，不吞内容。CSS 仍静态引入（23KB，换来首帧不闪）。 */
type KatexLike = {
  renderToString: (tex: string, opts?: Record<string, unknown>) => string;
};
let katexPromise: Promise<KatexLike> | null = null;
function loadKatex(): Promise<KatexLike> {
  if (!katexPromise) {
    katexPromise = import("katex").then(
      (m) => (m as { default?: KatexLike }).default ?? (m as unknown as KatexLike),
    );
  }
  return katexPromise;
}

function MathNode({ tex, display }: { tex: string; display?: boolean }) {
  const [html, setHtml] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    void loadKatex()
      .then((katex) => {
        try {
          const out = katex.renderToString(tex, {
            displayMode: display === true,
            throwOnError: false,
            strict: false,
            output: "html",
          });
          if (alive) setHtml(out);
        } catch {
          if (alive) setHtml(null);
        }
      })
      .catch(() => {
        if (alive) setHtml(null);
      });
    return () => {
      alive = false;
    };
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
   * 文档都复用它。那些位置一段话里能有十几个路径，每个前面顶一个方块，
   * 正文被切成拼贴画，可读性反而不如纯文本。只有最终答复正文
   * （`ToolCallBubble` 的 `kind === "text"` 步）显式打开。
   *
   * 图标本身是 **soft 形态**（淡染底 + 文字色字），不再品牌实色方块 ——
   * 见 PathLink 的注记。
   */
  pathIcons?: boolean;
  /**
   * 小节交互（复制本节 / 折叠）。**流式渲染传 false**：每个 chunk 重组小节树
   * 会让展开态反复重建；正文定型后再挂交互。
   */
  interactive?: boolean;
  /**
   * 是否处于流式生成中。为 `true` 且正文超过 `STREAM_TAIL_SPLIT_MIN` 时，
   * 只解析已闭合的前缀、最后一块按纯文本上屏（见 `splitStreamTail`）。
   * 定型后（`false`）恢复整段高质量渲染。
   */
  streaming?: boolean;
}

// satteri features: 对齐 react-markdown 的 remarkPlugins=[remarkGfm, remarkBreaks]
//   gfm:  tables / footnotes / strikethrough / task lists — 等价 remark-gfm
//   softBreaks: 由 @hermix/preview 的 hardenSoftLineBreaks 在字符串层处理
const SATTERI_FEATURES = { features: { gfm: true } } as const;

const MARKDOWN_REPARSE_THROTTLE_MS = 120;

/**
 * 流式长文的分块降级阈值（字符）。
 *
 * 低于这个长度整段解析最省事；超过之后全量重解析的代价开始咬人，于是只
 * 解析"已闭合的前缀"，正在生长的最后一块先按纯文本上屏（`md-stream-tail`）。
 * 6000 字是经验值：本仓库典型的一轮回答在 1000–4000 字，长报告才会过线。
 */
const STREAM_TAIL_SPLIT_MIN = 6000;

/**
 * 切出「稳定的头 + 正在长的尾」。
 *
 * 切点取最后一个空行：CommonMark 的块边界就在空行上，头部的解析结果因此
 * 是**稳定前缀**（只在新块出现时变化），可以整块复用；尾部按纯文本渲染，
 * 不必每 120ms 走一遍 satteri → hast → JSX。
 */
function splitStreamTail(md: string): { head: string; tail: string } {
  if (md.length < STREAM_TAIL_SPLIT_MIN) return { head: md, tail: "" };
  const idx = md.lastIndexOf("\n\n");
  // 切点太靠前（比如整段只有一个块）就不切：否则一半正文退回纯文本，
  // 观感反而更差。
  if (idx < 1500) return { head: md, tail: "" };
  return { head: md.slice(0, idx + 2), tail: md.slice(idx + 2) };
}

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
  content, onPathClick, pathIcons = false, interactive = true, streaming = false,
}: MarkdownBodyProps) {
  const handlePathClick = useCallback(onPathClick ?? (() => {}), [onPathClick]);
  // 主题判定走 useIsDarkTheme（响应式 + 不读已废除的 `.dark` 类）。
  // 早期版本用 readIsDark() 读 `.dark` 类，恒为 false —— 暗色主题下
  // 聊天代码块恒用亮色语法配色（punctuation 在 #1a1a1a 上仅 1.53:1）。
  const isDark = useIsDarkTheme();
  const displayContent = useThrottledValue(content, MARKDOWN_REPARSE_THROTTLE_MS);
  // 再套一层 deferred：解析（含 satteri → hast → JSX）发生在非紧急渲染里，
  // 流式期间不抢占输入框打字与滚动。120ms 节流决定"多久解析一次"，
  // deferred 决定"在哪个优先级解析"——两者互补。
  const deferredContent = useDeferredValue(displayContent);
  // 流式长回复：只解析**已闭合的块**（head），最后一段（正在长的那个块）
  // 先按纯文本上屏。理由：satteri 解析 + hast→JSX 的代价随正文线性增长，
  // 5000 字以上每 120ms 全量重解析就是几十毫秒的长任务；而正在增长的
  // 永远只是最后一块。head 是前缀，只有"多了一个空行"时才变 —— 于是
  // 完整解析的次数从"每个节流窗口一次"降到"每出现一个新块一次"。
  const { head, tail } = useMemo(
    () => (streaming ? splitStreamTail(deferredContent) : { head: deferredContent, tail: "" }),
    [deferredContent, streaming],
  );
  // 1. 不平衡 fence 关闭（与 react-markdown 时一致）
  // 2. 软换行 → 硬换行（satleri 不开 remarkBreaks，等价物在 strings 层）
  // 3. satteri 解析 → hast tree
  // 4. hast-util-to-jsx-runtime → React element
  const rendered = useMemo(() => {
    const normalized = normalizeWindowsImagePaths(head);
    const balanced = autoCloseUnbalancedFences(normalized);
    const hardened = hardenSoftLineBreaks(balanced);
    const withMath = transformMath(hardened);
    const hast = markdownToHast(withMath, SATTERI_FEATURES);
    // 图标去重作用域 = 本次解析（同一条消息）：同一文件名只有首个出现带
    // 文件类型图标，后续保持纯链接。流式期间每次 content 变更重建，
    // 语义稳定（当前版本的"首个"始终带图标）。
    const iconSeen = new Set<string>();
    return toJsxRuntime(hast, {
      Fragment,
      jsx,
      jsxs,
      components: {
        p({ children, ...props }: { children?: ReactNode }) {
          return <p {...props}>{linkifyChildren(children, handlePathClick, pathIcons, iconSeen)}</p>;
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
              {linkifyChildren(rest, handlePathClick, pathIcons, iconSeen)}
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
                  {linkifyChildren(cleaned, handlePathClick, pathIcons, iconSeen)}
                </div>
              </div>
            );
          }
          return <blockquote {...props}>{linkifyChildren(children, handlePathClick, pathIcons, iconSeen)}</blockquote>;
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
          //
          // `inline = true`：行内 code **不加粗**。药丸底色已经占了"这是
          // 代码"的重音，关键词再套一层 600 字重，两个重音互相抢 ——
          // 正文里一段话五个行内 code 就是五块"加粗带底色"的词，
          // 正是用户说的"界面有点乱"。代码块里没有底色，字重照旧。
          const isProp = (props as { prop?: string } | undefined)?.prop === "true";
          return (
            <code {...props} className="md-inline-code">
              {isProp
                ? highlightToReact(codeString, guessInlineLang(codeString), isDark, true)
                : children}
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
  }, [head, handlePathClick, pathIcons, isDark]);

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

  return (
    <div className="prose max-w-none">
      {body}
      {tail && <div className="md-stream-tail">{tail}</div>}
    </div>
  );
});
