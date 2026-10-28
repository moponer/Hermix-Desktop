// PPTX 渲染：pptx-browser 逐页画到 canvas，版面与滚动交给 DocPager。
//
// 必须按 slide 渲染：整篇转成一整块 HTML 没有分页概念，几十页的演示文稿
// 会连成一条长条。但"一页一页翻"同样不行——所以这里和 PDF 共用连续滚动
// 外壳：所有幻灯片铺成一列、滚到哪画到哪、左侧缩略图点一下跳过去。
//
// 已知离线代价：库内部会尝试从 Google Fonts 拉取替代字体，无网络时有 5s
// 硬超时兜底（不会卡死），首个文件可能略慢；Windows 自带的 Calibri/Cambria
// 等会被直接识别为系统字体而跳过该请求。

import { useEffect, useMemo, useRef, useState } from "react";
import { PptxRenderer } from "pptx-browser";
import { DocPager, type PageSize } from "./DocPager";

/** 一张幻灯片：宽度由外壳给定，渲染分辨率按 dpr 放大，CSS 尺寸定回逻辑像素。 */
function PptxSlideCanvas({
  renderer,
  slideIndex,
  width,
  height,
}: {
  renderer: PptxRenderer;
  slideIndex: number;
  width: number;
  height: number;
}) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  /** 同一张画布上的渲染串行化：库里没有取消接口，并发渲染会互相覆盖。 */
  const chainRef = useRef<Promise<void>>(Promise.resolve());

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || width <= 0) return;
    // 序号：连续缩放时旧的渲染直接作废（连画都不画），别把新尺寸盖回去。
    const seq = (canvas.dataset.seq = String(Number(canvas.dataset.seq ?? 0) + 1));
    const dpr = window.devicePixelRatio || 1;
    chainRef.current = chainRef.current.then(async () => {
      if (canvas.dataset.seq !== seq) return;
      try {
        await renderer.renderSlide(slideIndex, canvas, Math.round(width * dpr));
        if (canvas.dataset.seq !== seq) return;
        canvas.style.width = `${width}px`;
        canvas.style.height = `${height}px`;
      } catch {
        // 单页渲染失败保留上一帧，不整块崩
      }
    });
  }, [renderer, slideIndex, width, height]);

  return <canvas ref={canvasRef} />;
}

export function PptxView({ bytes }: { bytes: Uint8Array }) {
  const [loaded, setLoaded] = useState<{ renderer: PptxRenderer; count: number; w: number; h: number } | null>(
    null,
  );
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const renderer = new PptxRenderer();
    setError(null);
    setLoaded(null);

    (async () => {
      try {
        await renderer.load(new Uint8Array(bytes));
        if (cancelled) return;
        const { cx, cy } = renderer.slideSize;
        setLoaded({ renderer, count: renderer.slideCount, w: cx, h: cy });
      } catch (e) {
        if (!cancelled) setError(`演示文稿解析失败：${String(e)}`);
      }
    })();

    return () => {
      cancelled = true;
      renderer.destroy();
    };
  }, [bytes]);

  /** 所有幻灯片同一版面尺寸（presentation.xml 的 sldSz），按比例铺版。 */
  const sizes = useMemo<PageSize[]>(() => {
    if (!loaded) return [];
    return Array.from({ length: loaded.count }, () => ({ w: loaded.w, h: loaded.h }));
  }, [loaded]);

  if (error) return <div className="preview-state-error">{error}</div>;

  const render = (n: number, w: number, h: number) =>
    loaded ? (
      <PptxSlideCanvas renderer={loaded.renderer} slideIndex={n - 1} width={w} height={h} />
    ) : null;

  return (
    <DocPager
      count={loaded?.count ?? 0}
      sizes={sizes}
      loadingText="正在载入幻灯片…"
      renderPage={(n, w, h) => render(n, w, h)}
      renderThumb={(n, w, h) => render(n, w, h)}
    />
  );
}
