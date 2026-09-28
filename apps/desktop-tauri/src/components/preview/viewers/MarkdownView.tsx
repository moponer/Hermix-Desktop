// Markdown 渲染（satteri 管线）。
//
// 与聊天里的 MarkdownBody 差别：
//   - 不接 PathLink（预览的是本地文件，没有应用内路由）
//   - 外部 https:// 链接交给系统浏览器打开
//   - 不需要流式节流（预览一次性读全文）
//   - 左侧标题大纲（TOC）：≥3 个标题时显示，点击滚动定位

import { useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { ListTree, ChevronLeft, ChevronRight } from "lucide-react";
import { markdownToHast } from "satteri";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import { hardenSoftLineBreaks } from "../../../lib/softBreaks";
import { CodeBlock } from "../../MarkdownBody";
import { openExternalUrl } from "../../../desktopBridge";

interface TocEntry {
  level: number;
  text: string;
}

/// 提取 ATX 标题（# ~ ######），跳过围栏代码块内的 `#` 行。
/// setext 风格（=== / --- 下划线）少见且与分隔线歧义大，不取。
function extractHeadings(content: string): TocEntry[] {
  const entries: TocEntry[] = [];
  let inFence = false;
  for (const raw of content.split("\n")) {
    const line = raw.trimStart();
    if (/^(```|~~~)/.test(line)) {
      inFence = !inFence;
      continue;
    }
    if (inFence) continue;
    const m = /^(#{1,6})\s+(.+?)\s*#*\s*$/.exec(line);
    if (m) {
      // 行内代码/链接记号只影响展示，剥掉常见包裹符
      const text = m[2].replace(/[*`_]/g, "").trim();
      if (text) entries.push({ level: m[1].length, text });
    }
  }
  return entries;
}

export function MarkdownView({ content, isDark }: { content: string; isDark: boolean }) {
  const [tocOpen, setTocOpen] = useState(true);
  const articleRef = useRef<HTMLElement>(null);

  const headings = useMemo(() => extractHeadings(content), [content]);

  const rendered = useMemo<ReactNode>(() => {
    const hardened = hardenSoftLineBreaks(content);
    const hast = markdownToHast(hardened, { features: { gfm: true } });
    return toJsxRuntime(hast, {
      Fragment,
      jsx,
      jsxs,
      components: {
        a({ href, children, ...rest }: { href?: string; children?: ReactNode }) {
          return (
            <a
              href={href}
              onClick={(e) => {
                if (href && /^https?:\/\//i.test(href)) {
                  e.preventDefault();
                  openExternalUrl(href);
                }
              }}
              target="_blank"
              rel="noopener noreferrer"
              {...rest}
            >
              {children}
            </a>
          );
        },
        code({ className, children, ...props }: { className?: string; children?: ReactNode }) {
          const match = /language-([\w-]+)/.exec(className || "");
          const codeString = String(children ?? "").replace(/\n$/, "");
          if (match) {
            return (
              <CodeBlock language={match[1]!.toLowerCase()} code={codeString} isDark={isDark} />
            );
          }
          return <code {...props}>{children}</code>;
        },
      },
    });
  }, [content, isDark]);

  const scrollToHeading = (idx: number) => {
    // 渲染树中的标题与提取顺序一致（围栏内 # 已排除）；按文档顺序取第 idx 个
    const nodes = articleRef.current?.querySelectorAll("h1,h2,h3,h4,h5,h6");
    nodes?.[idx]?.scrollIntoView({ behavior: "smooth", block: "start" });
  };

  const showToc = headings.length >= 3;

  return (
    // 布局契约：host 是「固定 TOC 列 + 独立滚动正文列」的 flex 行。
    // TOC 不在滚动流里 —— 滚动的只有正文列，目录钉死在左侧不跟正文跑。
    <div className={showToc ? "preview-md-layout" : "preview-md-host"}>
      {showToc && (
        <>
          {tocOpen && (
            <nav className="preview-md-toc" aria-label="标题大纲">
              <div className="preview-md-toc-head">
                <ListTree className="icon-sm" />
                <span>目录</span>
              </div>
              <div className="preview-md-toc-list">
                {headings.map((h, i) => (
                  <button
                    key={i}
                    type="button"
                    className="preview-md-toc-item"
                    style={{ paddingLeft: `${(h.level - 1) * 12 + 8}px` }}
                    title={h.text}
                    onClick={() => scrollToHeading(i)}
                  >
                    {h.text}
                  </button>
                ))}
              </div>
            </nav>
          )}
          <button
            type="button"
            className="preview-md-toc-toggle"
            title={tocOpen ? "收起目录" : "展开目录"}
            aria-label={tocOpen ? "收起目录" : "展开目录"}
            onClick={() => setTocOpen((o) => !o)}
          >
            {tocOpen ? <ChevronLeft className="icon-sm" /> : <ChevronRight className="icon-sm" />}
          </button>
        </>
      )}
      <div className="preview-md-scroll">
        <article ref={articleRef} className="preview-md-body">{rendered}</article>
      </div>
    </div>
  );
}
