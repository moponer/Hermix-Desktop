/**
 * CodeView — virtualized line numbers + refractor highlighting.
 *
 * The line-number gutter is virtualized: only the visible rows ± a
 * buffer are rendered, so a 5000-line file mounts ~60 spans instead of
 * N. Highlighting is delegated to the shared `src/lib/codeHighlight.ts`
 * engine used by both MarkdownBody (chat) and CodeView (preview), so the
 * two renderers never drift apart.
 */
import { useMemo, useRef, useState, useEffect, useLayoutEffect, useCallback } from "react";
import { clsx } from "clsx";
import { Check, Copy, X } from "lucide-react";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { highlightToReact } from "../lib/codeHighlight";
import { useIsDarkTheme } from "../hooks/useTheme";
import { COPIED, COPY_FAILED } from "./strings";

interface CodeViewProps {
  source: string;
  language: string | null;
  wrap?: boolean;
  /** 是否暗色主题。不传则跟随当前主题（响应式）。
   *  控制使用 ONE_DARK 还是 ONE_LIGHT 配色。 */
  isDark?: boolean;
  activeLine?: number;
  onLineClick?: (line: number) => void;
}

/** 数 \n —— 比 splitLines 快 5-10 倍，大文件不卡 */
function countLines(source: string): number {
  if (!source) return 0;
  let n = 1;
  for (let i = 0; i < source.length; i++) {
    if (source.charCodeAt(i) === 10) n++;
  }
  if (source.endsWith("\n")) n--;
  return Math.max(1, n);
}

// ── Virtualized Line Number Gutter ──────────────────────────
// Only renders line numbers in the visible scroll region ± buffer.
// Driven by the scroll position of the parent `.code-viewport`.

const BUFFER_LINES = 15; // render 15 extra lines above/below viewport
const LINE_HEIGHT_FALLBACK = 20.8; // 13px * 1.6 — used before the first measurement lands

interface GutterProps {
  lineCount: number;
  activeLine: number;
  onLineClick?: (line: number) => void;
}

function VirtualGutter({ lineCount, activeLine, onLineClick }: GutterProps) {
  const gutterRef = useRef<HTMLDivElement>(null);
  const [visibleRange, setVisibleRange] = useState({ start: 0, end: Math.min(lineCount, 80) });
  const [lineHeight, setLineHeight] = useState(LINE_HEIGHT_FALLBACK);
  // Mirror `lineHeight` into a ref so the scroll handler closure below
  // can read the latest value without re-subscribing the scroll listener
  // every time the measurement ticks. Re-subscribing would cause one frame
  // of missed scroll events per measurement.
  const lineHeightRef = useRef(lineHeight);
  lineHeightRef.current = lineHeight;

  // Measure the actual rendered height of one gutter row. This runs once
  // after the first paint (when at least one <span class="code-ln"> exists)
  // and again whenever the visible range changes, because previously rendered
  // spans get unmounted and remounted. We also re-measure on viewport
  // resize (the existing ResizeObserver effect handles that case via the
  // lineCount dep below).
  // Why measure instead of hardcode: the body's font-size × line-height is
  // 13 * 1.6 = 20.8px in the current CSS, but font availability on the
  // user's machine, sub-pixel rounding, and any future CSS tweaks can
  // shift that. A measurement eliminates the drift entirely.
  useLayoutEffect(() => {
    const gutter = gutterRef.current;
    if (!gutter) return;
    const first = gutter.querySelector(".code-ln") as HTMLElement | null;
    if (!first) return;
    const h = first.getBoundingClientRect().height;
    if (h > 0 && Math.abs(h - lineHeight) > 0.05) {
      setLineHeight(h);
    }
  }, [visibleRange.start, visibleRange.end, lineCount, lineHeight]);

  // Sync gutter scroll position with the code viewport's scroll.
  // We observe the `.code-viewport` (the scroll container) and read
  // its scrollTop on each scroll event to compute the visible range.
  useEffect(() => {
    const gutter = gutterRef.current;
    if (!gutter) return;
    // The scroll container is the `.code-viewport` parent.
    const viewport = gutter.closest(".code-viewport") as HTMLElement | null;
    if (!viewport) return;

    let rafId = 0;
    const update = () => {
      const scrollTop = viewport.scrollTop;
      const viewportHeight = viewport.clientHeight;
      // Read the measured row height from the ref so the closure always
      // uses the latest value without forcing a re-subscribe on every
      // measurement update. Fallback constant covers the very first paint
      // before any <span class="code-ln"> exists yet.
      const lh = lineHeightRef.current || LINE_HEIGHT_FALLBACK;
      const startLine = Math.max(0, Math.floor(scrollTop / lh) - BUFFER_LINES);
      const endLine = Math.min(
        lineCount,
        Math.ceil((scrollTop + viewportHeight) / lh) + BUFFER_LINES,
      );
      setVisibleRange((prev) => {
        // Avoid re-render if range hasn't changed significantly
        if (prev.start === startLine && prev.end === endLine) return prev;
        return { start: startLine, end: endLine };
      });
    };

    const onScroll = () => {
      cancelAnimationFrame(rafId);
      rafId = requestAnimationFrame(update);
    };

    viewport.addEventListener("scroll", onScroll, { passive: true });
    // Initial calculation
    update();

    // Also observe resize to recalculate when the viewport changes size
    const ro = new ResizeObserver(() => {
      cancelAnimationFrame(rafId);
      rafId = requestAnimationFrame(update);
    });
    ro.observe(viewport);

    return () => {
      viewport.removeEventListener("scroll", onScroll);
      ro.disconnect();
      cancelAnimationFrame(rafId);
    };
  }, [lineCount]);

  const { start, end } = visibleRange;

  return (
    <div
      ref={gutterRef}
      className="code-gutter"
      aria-hidden="true"
      style={{
        // The gutter must have the same total height as the code body
        // so the scroll positions match. We use a padding-based approach:
        // top padding = space for non-rendered lines above,
        // bottom padding = space for non-rendered lines below.
        // `lineHeight` is the measured row height of one .code-ln span —
        // see the useLayoutEffect above — so it tracks whatever the
        // browser actually renders (font, sub-pixel rounding, future
        // CSS tweaks) instead of a stale hardcoded constant.
        paddingTop: start * lineHeight,
        paddingBottom: Math.max(0, (lineCount - end) * lineHeight),
      }}
    >
      {Array.from({ length: end - start }, (_, i) => {
        const ln = start + i + 1;
        return (
          <span
            key={ln}
            className="code-ln"
            data-line={ln}
            data-active={activeLine === ln ? "true" : undefined}
            onClick={() => onLineClick?.(ln)}
          >
            {ln}
          </span>
        );
      })}
    </div>
  );
}

// ── Non-virtualized Gutter (for small files < 300 lines) ──
// Simpler, no scroll listener overhead for small files.

function SimpleGutter({ lineCount, activeLine, onLineClick }: GutterProps) {
  return (
    <div className="code-gutter" aria-hidden="true">
      {Array.from({ length: lineCount }, (_, i) => {
        const ln = i + 1;
        return (
          <span
            key={ln}
            className="code-ln"
            data-line={ln}
            data-active={activeLine === ln ? "true" : undefined}
            onClick={() => onLineClick?.(ln)}
          >
            {ln}
          </span>
        );
      })}
    </div>
  );
}

// ── Main CodeView Component ────────────────────────────────

const VIRTUAL_THRESHOLD = 300; // Use virtual gutter for files > 300 lines

export function CodeView({
  source,
  language,
  wrap = false,
  isDark,
  activeLine = 0,
  onLineClick,
}: CodeViewProps) {
  const safeSetTimeout = useSafeTimeout();
  const [copied, setCopied] = useState(false);
  const [failed, setFailed] = useState(false);

  // 未传 isDark 时跟随当前主题。useIsDarkTheme 是响应式的：主题切换后
  // 重新取值 → useMemo 依赖 dark 变 → 重新 tokenize（换配色）。
  // 旧的 `isDark ?? readIsDark()` 读的是已废除的 `.dark` 类，恒为 false。
  const themeIsDark = useIsDarkTheme();
  const dark = isDark ?? themeIsDark;

  // 同步渲染：高亮 token 树。refractor 是同步 API，useMemo 缓存。
  // 切文件 → source/language 变 → 自动重新 tokenize。
  // 切主题 → isDark 变 → 重新 tokenize（换配色）。
  const highlighted = useMemo(
    () => highlightToReact(source, language, dark),
    [source, language, dark],
  );

  const lineCount = useMemo(() => countLines(source), [source]);

  // Memoize the line click handler to avoid re-renders
  const handleLineClick = useCallback((ln: number) => {
    onLineClick?.(ln);
  }, [onLineClick]);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(source);
      setCopied(true);
      setFailed(false);
      safeSetTimeout(() => setCopied(false), 1500);
    } catch {
      setFailed(true);
      setCopied(false);
      safeSetTimeout(() => setFailed(false), 1500);
    }
  }, [source, safeSetTimeout]);

  if (!source) {
    return <div className="code-empty">空文件</div>;
  }

  // Virtualization assumes "1 logical line == 1 visual row". When `wrap`
  // is on, a long logical line can break across multiple visual rows,
  // which makes the lineHeight-based padding math wrong (and the
  // scrollTop→startLine mapping wrong too). Force the simple gutter in
  // that case. Currently no caller passes wrap=true, but the guard
  // prevents a future caller from silently getting a misaligned gutter.
  const useVirtual = !wrap && lineCount > VIRTUAL_THRESHOLD;

  return (
    <div
      className={clsx("code-viewport", wrap && "is-wrap")}
      data-active-line={activeLine > 0 ? activeLine : undefined}
      onClick={(e) => {
        // Fallback click handler for non-virtual gutter or
        // clicks on the code body itself.
        const target = e.target as HTMLElement;
        const lnEl = target.closest?.("[data-line]") as HTMLElement | null;
        if (lnEl && onLineClick) {
          const ln = Number(lnEl.dataset.line);
          if (Number.isFinite(ln)) onLineClick(ln);
        }
      }}
    >
      {/* 复制按钮 — hover 时显示，复制全部源代码 */}
      <button
        type="button"
        className="code-copy-btn"
        onClick={(e) => {
          e.stopPropagation();
          handleCopy();
        }}
        title={copied ? COPIED : failed ? COPY_FAILED : "复制全部代码"}
        aria-label={copied ? COPIED : failed ? COPY_FAILED : "复制全部代码"}
      >
        {copied ? <Check className="icon-md" /> : failed ? <X className="icon-md" /> : <Copy className="icon-md" />}
      </button>

      <div className="code-flex">
        {/* 行号 gutter — virtualized for large files */}
        {useVirtual ? (
          <VirtualGutter
            lineCount={lineCount}
            activeLine={activeLine}
            onLineClick={handleLineClick}
          />
        ) : (
          <SimpleGutter
            lineCount={lineCount}
            activeLine={activeLine}
            onLineClick={handleLineClick}
          />
        )}

        {/* 代码主体 —— 一次性高亮的 token 树 */}
        <pre className="code-pre">
          <code
            className={clsx("code-body", language ? `language-${language}` : "language-text")}
          >
            {highlighted}
          </code>
        </pre>
      </div>
    </div>
  );
}
