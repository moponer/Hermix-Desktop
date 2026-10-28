// 连续滚动文档的共用外壳：PDF / PPTX 这类"一页一张 canvas"的渲染器都走它。
// （Word 走自己那一套：docx-preview 一次出整篇 DOM，不能按页重画。）
//
// 结构：工具条（页码 / 前后翻 / 缩略图开关 / 缩放 / 额外动作）
//      正文区 = [缩略图侧栏] + 纵向页列
//
// 两条规矩写死在这里，别在各渲染器里重实现：
//   1) 一页一页翻是没有的——所有页铺成一列连续滚动，当前页由滚动位置反推；
//   2) 视口外一屏的页不画（IntersectionObserver），滚到了才渲。缩略图同理，
//      侧栏滚到哪画到哪——一份 50 页的 PPT 不该为了缩略图先渲 50 遍。
//
// 渲染器只需要回答两个问题：某页在给定宽度下怎么画（renderPage），
// 以及给定小宽度下的缩略图怎么画（renderThumb）。

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";
import { ChevronLeft, ChevronRight, PanelLeft, RotateCw, ZoomIn, ZoomOut } from "lucide-react";
import { usePageScroll, type PageAnchor } from "../lib/usePageScroll";

const MIN_SCALE = 0.5;
const MAX_SCALE = 3;
const ZOOM_STEP = 0.25;
/** 缩略图渲染宽度（CSS px）：与 .docpager-thumbs 的内容宽对齐，写死免得量两次布局。 */
export const THUMB_WIDTH = 104;
/** 视口外多远就开始画：一屏的距离足够让滚动不出现白块。 */
const PRE_RENDER_MARGIN = "800px 0px";

/** 一页的原始尺寸（任意单位，只用比例与最大宽算"适应宽度"）。 */
export interface PageSize {
  w: number;
  h: number;
}

/**
 * 进入视口附近才渲染的占位盒子。尺寸先占好，内容后到——滚到哪画到哪。
 * 缩略图侧栏里也用它：侧栏自己的滚动会让"被祖先裁剪后的可见区域"变化，
 * 所以 root 用默认的视口就够（IntersectionObserver 会算上祖先的裁剪）。
 */
export function NearSlot({
  width,
  height,
  children,
}: {
  width: number;
  height: number;
  children: ReactNode;
}) {
  const ref = useRef<HTMLDivElement | null>(null);
  const [near, setNear] = useState(false);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) setNear(true);
      },
      { rootMargin: "200px 0px" },
    );
    io.observe(el);
    return () => io.disconnect();
  }, []);

  const style: CSSProperties = { width, height };
  return (
    <div ref={ref} className="docpager-thumb-slot" style={style}>
      {near ? children : null}
    </div>
  );
}

export interface DocPagerProps {
  count: number;
  /** 每页 scale=1 时的尺寸；空数组 = 还没准备好（显示 loadingText）。 */
  sizes: PageSize[];
  /** 画一页：宽度/高度已由外壳算好（CSS px）。 */
  renderPage: (pageNo: number, width: number, height: number, rotation: number) => ReactNode;
  /** 画缩略图：宽度固定 THUMB_WIDTH，高度按页比例。 */
  renderThumb: (pageNo: number, width: number, height: number, rotation: number) => ReactNode;
  /** 工具条上是否给"旋转 90°"（PDF 有，PPTX 没有）。 */
  rotatable?: boolean;
  loadingText?: string;
}

export function DocPager({
  count,
  sizes,
  renderPage,
  renderThumb,
  rotatable = false,
  loadingText = "正在载入页面…",
}: DocPagerProps) {
  const [stageEl, setStageEl] = useState<HTMLDivElement | null>(null);
  const [stageWidth, setStageWidth] = useState(0);
  const [scale, setScale] = useState(1);
  const [rotation, setRotation] = useState(0);
  const [thumbsOpen, setThumbsOpen] = useState(false);
  /** 已进入过预渲染范围的页（只增不减：画过的页滚回来不重画）。 */
  const [painted, setPainted] = useState<ReadonlySet<number>>(() => new Set());
  const thumbsInit = useRef(false);
  const anchorRef = useRef<PageAnchor | null>(null);
  const thumbEls = useRef<Array<HTMLButtonElement | null>>([]);

  const { current, register, els, scrollTo, capture, restore } = usePageScroll(stageEl, count);

  // 用户旋转 90°/270° 时宽高互换——基准尺寸是"页面自带旋转后"的，别在这里再叠一次。
  const turned = rotation === 90 || rotation === 270;
  const dims = sizes.map((s) => (turned ? { w: s.h, h: s.w } : s));
  const maxW = dims.reduce((m, d) => Math.max(m, d.w), 0);
  /** scale=1 时"适应宽度"的比例：先按容器定基准，再乘用户缩放。 */
  const fit = maxW > 0 && stageWidth > 0 ? stageWidth / maxW : 0;

  // 换文档（sizes 换了新数组）：已画页集合与缩放/旋转回到初始，
  // 别让上一份文件的状态漏过来（同一份文档缩放时 sizes 不变）。
  useEffect(() => {
    setPainted(new Set());
    setScale(1);
    setRotation(0);
  }, [sizes]);

  // 容器宽度（桌面右栏收窄、全屏铺开、缩略图开关都会走到这里）。
  useEffect(() => {
    if (!stageEl) return;
    const sync = () => setStageWidth(stageEl.clientWidth - 32); // 左右各 16 内边距
    sync();
    const ro = new ResizeObserver(sync);
    ro.observe(stageEl);
    return () => ro.disconnect();
  }, [stageEl]);

  // 缩略图侧栏默认值：宽面板（全屏 / 独立查看器）给，窄栏（桌面右栏）不给
  // ——右栏里侧栏会吃掉正文一半宽。只判一次，之后由用户自己开关。
  useEffect(() => {
    if (thumbsInit.current || stageWidth === 0) return;
    thumbsInit.current = true;
    if (stageWidth >= 520) setThumbsOpen(true);
  }, [stageWidth]);

  // 惰性渲染：一个 observer 管所有页，进过视野就永久算"画过"。
  useEffect(() => {
    if (!stageEl) return;
    const io = new IntersectionObserver(
      (entries) => {
        const add: number[] = [];
        for (const e of entries) {
          if (!e.isIntersecting) continue;
          const n = Number((e.target as HTMLElement).dataset.page);
          if (n > 0) add.push(n);
        }
        if (add.length === 0) return;
        setPainted((prev) => {
          if (add.every((n) => prev.has(n))) return prev;
          const next = new Set(prev);
          for (const n of add) next.add(n);
          return next;
        });
      },
      { root: stageEl, rootMargin: PRE_RENDER_MARGIN },
    );
    for (const el of els.current) if (el) io.observe(el);
    return () => io.disconnect();
    // fit：页列铺出来之前没有节点可观察，铺出来后要重新 observe 一轮。
    // sizes：换文档时节点原地复用，旧 observer 不会再报一次，必须重建。
  }, [stageEl, els, fit, count, sizes]);

  // 当前页的缩略图跟着滚进可视区（block:"nearest"：已经看得见就不动）。
  useEffect(() => {
    if (!thumbsOpen) return;
    thumbEls.current[current - 1]?.scrollIntoView({ block: "nearest" });
  }, [current, thumbsOpen]);

  const zoomStep = useCallback(
    (dir: 1 | -1) => {
      const next = Math.min(MAX_SCALE, Math.max(MIN_SCALE, +(scale + dir * ZOOM_STEP).toFixed(2)));
      // 已到上下限就直接返回：别留一个永不消费的锚点，否则下一次重排
      // （改窗口宽度）会拿旧锚点把滚动位置拽回去。
      if (next === scale) return;
      anchorRef.current = capture();
      setScale(next);
    },
    [scale, capture],
  );

  const rotate = useCallback(() => {
    anchorRef.current = capture();
    setRotation((r) => (r + 90) % 360);
  }, [capture]);

  // 铺版/缩放/旋转落定后按锚点还原视口（首帧没有锚点，跳过）。
  const layoutKey = `${fit.toFixed(4)}|${scale}|${rotation}`;
  useLayoutEffect(() => {
    restore(anchorRef.current);
    anchorRef.current = null;
  }, [layoutKey, restore]);

  // Ctrl+滚轮缩放：React 的 wheel 委托是 passive 的，preventDefault 无效，
  // 必须原生监听 { passive: false } 才能拦下浏览器的页面缩放。
  useEffect(() => {
    if (!stageEl) return;
    const onWheel = (e: WheelEvent) => {
      if (!e.ctrlKey) return;
      e.preventDefault();
      zoomStep(e.deltaY < 0 ? 1 : -1);
    };
    stageEl.addEventListener("wheel", onWheel, { passive: false });
    return () => stageEl.removeEventListener("wheel", onWheel);
  }, [stageEl, zoomStep]);

  // 键盘快捷缩放：挂 window 而不是画布——画布上没有焦点概念，焦点在
  // 文件列表里时也该生效。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (!e.ctrlKey || e.altKey || e.metaKey) return;
      if (e.key === "=" || e.key === "+") {
        e.preventDefault();
        zoomStep(1);
      } else if (e.key === "-") {
        e.preventDefault();
        zoomStep(-1);
      } else if (e.key === "0") {
        e.preventDefault();
        if (scale === 1 && rotation === 0) return;
        anchorRef.current = capture();
        setScale(1);
        setRotation(0);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [zoomStep, capture, scale, rotation]);

  const ready = count > 0 && dims.length === count && fit > 0;

  return (
    <div className="preview-pdf">
      <div className="preview-pdf-bar">
        <button
          type="button"
          className="head-action"
          disabled={current <= 1}
          onClick={() => scrollTo(current - 1)}
          aria-label="上一页"
          title="上一页"
        >
          <ChevronLeft className="icon-md" />
        </button>
        <span className="preview-pdf-page">{count ? `${current} / ${count}` : "载入中…"}</span>
        <button
          type="button"
          className="head-action"
          disabled={count === 0 || current >= count}
          onClick={() => scrollTo(current + 1)}
          aria-label="下一页"
          title="下一页"
        >
          <ChevronRight className="icon-md" />
        </button>
        <button
          type="button"
          className={thumbsOpen ? "head-action is-active" : "head-action"}
          onClick={() => setThumbsOpen((v) => !v)}
          aria-pressed={thumbsOpen}
          aria-label={thumbsOpen ? "隐藏页面缩略图" : "显示页面缩略图"}
          title={thumbsOpen ? "隐藏页面缩略图" : "显示页面缩略图（点缩略图跳页）"}
        >
          <PanelLeft className="icon-md" />
        </button>
        <span className="preview-pdf-spacer" />
        <button
          type="button"
          className="head-action"
          onClick={() => zoomStep(-1)}
          aria-label="缩小（Ctrl+- 或 Ctrl+滚轮）"
          title="缩小（Ctrl+- 或 Ctrl+滚轮）"
        >
          <ZoomOut className="icon-md" />
        </button>
        <span className="preview-pdf-zoom">{Math.round(scale * 100)}%</span>
        <button
          type="button"
          className="head-action"
          onClick={() => zoomStep(1)}
          aria-label="放大（Ctrl+= 或 Ctrl+滚轮）"
          title="放大（Ctrl+= 或 Ctrl+滚轮）"
        >
          <ZoomIn className="icon-md" />
        </button>
        {rotatable && (
          <button
            type="button"
            className="head-action"
            onClick={rotate}
            aria-label={`旋转 90°（当前 ${rotation}°）`}
            title="顺时针旋转 90°（Ctrl+0 复位）"
          >
            <RotateCw className="icon-md" />
          </button>
        )}
      </div>

      <div className="docpager-body">
        {thumbsOpen && ready && (
          <div className="docpager-thumbs" role="group" aria-label="页面缩略图">
            {dims.map((d, i) => {
              const n = i + 1;
              const th = Math.round(THUMB_WIDTH * (d.h / d.w));
              return (
                <button
                  key={n}
                  ref={(el) => {
                    thumbEls.current[i] = el;
                  }}
                  type="button"
                  className={n === current ? "docpager-thumb is-active" : "docpager-thumb"}
                  onClick={() => scrollTo(n)}
                  aria-label={`跳到第 ${n} 页`}
                  aria-current={n === current ? "page" : undefined}
                >
                  <NearSlot width={THUMB_WIDTH} height={th}>
                    {renderThumb(n, THUMB_WIDTH, th, rotation)}
                  </NearSlot>
                  <span className="docpager-thumb-no">{n}</span>
                </button>
              );
            })}
          </div>
        )}

        <div className="preview-pdf-stage" ref={setStageEl}>
          {ready ? (
            <div className="docpager-pages">
              {dims.map((d, i) => {
                const n = i + 1;
                const w = Math.round(d.w * fit * scale);
                const h = Math.round(d.h * fit * scale);
                return (
                  <div
                    key={n}
                    ref={register(i)}
                    className="docpager-sheet"
                    style={{ width: w, height: h }}
                    data-page={n}
                  >
                    {painted.has(n) ? renderPage(n, w, h, rotation) : null}
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="preview-state-center">{loadingText}</div>
          )}
        </div>
      </div>
    </div>
  );
}
