// Markdown 预览（预览面板专用）。
//
// 与聊天里的 MarkdownBody 的分工：聊天正文是流式渲染、要点路径跳转、
// 要接 PathLink；预览的是**磁盘上一个已经写完的文件**，因此这里的重点
// 完全不同 —— 版面要像一份文档（行长、层级、表格、脚注），而且正文里的
// 相对路径必须能落到真实文件。
//
// 与旧实现相比修掉的四件事：
//   1. rawHtml 之前没开，Markdown 里内嵌的 HTML 整段被吞掉（satteri 默认
//      把它当不透明 raw 节点，toJsxRuntime 渲染不出来）。
//   2. frontmatter（--- … ---）之前会当正文渲染出一坨元数据。
//   3. 相对路径图片之前直接裂图：浏览器拿应用源去解析 `img/x.png`。现在
//      按文档目录补全后走产物读取链（那条链已带产物根白名单）。
//   4. 行宽写死 780px，窄侧栏里根本用不上、全屏又太宽。改由 CSS 按字符数
//      约束，两种场景都落在可读区间。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { ReactNode } from "react";
import { ListTree, ChevronLeft, ChevronRight } from "lucide-react";
import { markdownToHast } from "satteri";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import type { FileSource } from "../lib/fileSource";
import { dirnameOf, isExternalHref, resolveRelative } from "../lib/localAssets";
import { stripFrontmatter, extractHeadings } from "../lib/markdownDoc";
import { CodeBlock } from "../lib/CodeBlock";

/** 解析开关。GFM 全开（表格/删除线/任务列表/脚注），rawHtml 修掉内嵌 HTML 被吞。 */
const FEATURES = { features: { gfm: true, rawHtml: true } } as const;

/** 单张图片上限，与图片预览同一档，避免一张巨图把整页拖垮。 */
const INLINE_ASSET_LIMIT = 8 * 1024 * 1024;

const MIME_BY_EXT: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  svg: "image/svg+xml",
  ico: "image/x-icon",
  avif: "image/avif",
};

type HastNode = {
  type: string;
  tagName?: string;
  properties?: Record<string, unknown>;
  children?: HastNode[];
};

/** 收集正文里全部带 src 的图片节点。 */
function collectImages(root: HastNode, into: HastNode[] = []): HastNode[] {
  if (root.type === "element" && (root.tagName === "img" || root.tagName === "image")) {
    if (typeof root.properties?.src === "string") into.push(root);
  }
  for (const child of root.children ?? []) collectImages(child, into);
  return into;
}

/** 按扩展名猜 MIME；猜不到就不内联，交回 CSS 占位。 */
function mimeFor(path: string): string | null {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  return MIME_BY_EXT[ext] ?? null;
}

/**
 * 把"用户点击站内链接"的拦截表达：
 *   - 文档内锚点（href 以 `#` 开头）让浏览器自己跳；
 *   - 其余统统交给 FileSource 实现层（desktop → 系统浏览器；viewer →
 *     `revealInExplorer` / 由调用方在 source 上提供 `openExternalUrl`）。
 *
 * 这里只关心"什么算外部"，细节由 source.fs / 系统兜底。
 */
async function followExternal(
  source: FileSource,
  resolvedUrl: string,
): Promise<void> {
  if (source.openWithSystem) {
    const r = await source.openWithSystem(resolvedUrl);
    if (r.ok) return;
  }
  // 兜底：试图打开浏览器侧导航（新窗口）—— Tauri Webview 行为未保证。
  if (typeof window !== "undefined") {
    window.open(resolvedUrl, "_blank", "noopener,noreferrer");
  }
}

export function MarkdownView({
  content,
  source,
  isDark,
  resolvedPath,
  zoom,
}: {
  content: string;
  source: FileSource;
  isDark: boolean;
  resolvedPath: string | null;
  zoom: number;
}) {
  const [tocOpen, setTocOpen] = useState(true);
  const [activeHeading, setActiveHeading] = useState(-1);
  const articleRef = useRef<HTMLElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  const body = useMemo(() => stripFrontmatter(content), [content]);
  const headings = useMemo(() => extractHeadings(body), [body]);

  // 相对路径图片 → blob URL。整篇共用一份，卸载时统一回收。
  const [assets, setAssets] = useState<ReadonlyMap<string, string>>(new Map());
  const blobUrlsRef = useRef<string[]>([]);

  useEffect(() => {
    const dir = resolvedPath ? dirnameOf(resolvedPath) : null;
    if (!dir) {
      setAssets(new Map());
      return;
    }
    let cancelled = false;
    const pending = collectImages(markdownToHast(body, FEATURES)).filter(
      (n) => !isExternalHref(n.properties?.src as string),
    );
    if (pending.length === 0) {
      setAssets(new Map());
      return;
    }
    void Promise.all(
      pending.map(async (node) => {
        const raw = node.properties?.src as string;
        const abs = resolveRelative(dir, raw);
        const mime = mimeFor(abs);
        if (!mime) return null;
        const res = await source.readBytes(abs);
        if (!res.ok) return null;
        if (res.bytes.byteLength > INLINE_ASSET_LIMIT) return null;
        // 复制一份到 ArrayBuffer（不是 SharedArrayBuffer 视图）—— 现代 TS DOM
        // 把 `BlobPart` 收紧到 `ArrayBufferView<ArrayBuffer>`，直接传
        // `Uint8Array<ArrayBufferLike>` 会被拒收。
        const copy = new Uint8Array(res.bytes.byteLength);
        copy.set(res.bytes);
        return { raw, url: URL.createObjectURL(new Blob([copy], { type: mime })) };
      }),
    ).then((results) => {
      if (cancelled) return;
      const next = new Map<string, string>();
      for (const r of results) {
        if (r) {
          next.set(r.raw, r.url);
          blobUrlsRef.current.push(r.url);
        }
      }
      setAssets(next);
    });
    return () => {
      cancelled = true;
    };
  }, [body, resolvedPath, source]);

  useEffect(
    () => () => {
      for (const url of blobUrlsRef.current) URL.revokeObjectURL(url);
      blobUrlsRef.current = [];
    },
    [],
  );

  const rendered = useMemo<ReactNode>(() => {
    const hast = markdownToHast(body, FEATURES);
    const dir = resolvedPath ? dirnameOf(resolvedPath) : "";
    return toJsxRuntime(hast, {
      Fragment,
      jsx,
      jsxs,
      components: {
        a({ href, children, ...rest }: { href?: string; children?: ReactNode }) {
          if (!href) return <a {...rest}>{children}</a>;
          return (
            <a
              href={href}
              target="_blank"
              rel="noopener noreferrer"
              onClick={(e) => {
                e.preventDefault();
                // 文档内锚点由浏览器自己跳；其余一律交给系统浏览器，
                // 相对路径先按文档目录补全，否则系统也找不到文件。
                if (!href.startsWith("#")) {
                  void followExternal(source, resolveRelative(dir, href));
                }
              }}
              {...rest}
            >
              {children}
            </a>
          );
        },
        img({ src, alt, ...rest }: { src?: string; alt?: string }) {
          if (!src) return null;
          return <img src={assets.get(src) ?? src} alt={alt ?? ""} loading="lazy" {...rest} />;
        },
        code({ className, children, ...props }: { className?: string; children?: ReactNode }) {
          const match = /language-([\w-]+)/.exec(className || "");
          const codeString = String(children ?? "").replace(/\n$/, "");
          if (match) {
            return <CodeBlock language={match[1]!.toLowerCase()} code={codeString} isDark={isDark} />;
          }
          return <code {...props}>{children}</code>;
        },
        // GFM 任务列表在预览里只读：勾选状态来自文件，能点却改不落盘会骗人。
        input(props: { type?: string; checked?: boolean }) {
          return <input {...props} disabled />;
        },
      },
    });
  }, [body, isDark, assets, resolvedPath, source]);

  // 目录高亮：取当前滚动位置之上的最后一个标题。
  const syncActive = useCallback(() => {
    const nodes = articleRef.current?.querySelectorAll("h1,h2,h3,h4,h5,h6");
    if (!nodes || nodes.length === 0) return;
    let current = -1;
    for (let i = 0; i < nodes.length; i++) {
      if (nodes[i].getBoundingClientRect().top <= 80) current = i;
    }
    setActiveHeading((prev) => (prev === current ? prev : current));
  }, []);

  useEffect(() => {
    syncActive();
  }, [syncActive, rendered]);

  const scrollToHeading = useCallback((idx: number) => {
    const nodes = articleRef.current?.querySelectorAll("h1,h2,h3,h4,h5,h6");
    nodes?.[idx]?.scrollIntoView({ behavior: "smooth", block: "start" });
  }, []);

  const showToc = headings.length >= 3;

  return (
    // 布局契约：host 是"固定目录列 + 独立滚动正文列"的 flex 行。
    // 目录不在滚动流里 —— 滚动的只有正文列。
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
                    className={
                      i === activeHeading ? "preview-md-toc-item is-active" : "preview-md-toc-item"
                    }
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
      <div className="preview-md-scroll" ref={scrollRef} onScroll={syncActive}>
        <div className="preview-md-zoom" style={{ zoom: zoom / 100 }}>
          <article ref={articleRef} className="preview-md-body">
            {rendered}
          </article>
        </div>
      </div>
    </div>
  );
}
