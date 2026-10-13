// PPTX 渲染：pptx-browser 逐页画到 canvas。
//
// 必须按 slide 渲染：整篇转成一整块 HTML 没有分页概念，几十页的演示
// 文稿会连成一条长条，没法翻页。这里带页码导航与缩放（缩放通过调整
// 渲染宽度实现，canvas 保持清晰）。
//
// 已知离线代价：库内部会尝试从 Google Fonts 拉取替代字体，无网络时
// 有 5s 硬超时兜底（不会卡死），首个文件可能略慢；Windows 自带的
// Calibri/Cambria 等会被直接识别为系统字体而跳过该请求。

import { useCallback, useEffect, useRef, useState } from "react";
import { PptxRenderer } from "pptx-browser";
import { ChevronLeft, ChevronRight, ZoomIn, ZoomOut } from "lucide-react";

const MIN_WIDTH = 480;
const MAX_WIDTH = 1920;

export function PptxView({ bytes }: { bytes: Uint8Array }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const rendererRef = useRef<PptxRenderer | null>(null);

  const [slideCount, setSlideCount] = useState(0);
  const [slide, setSlide] = useState(0);
  const [width, setWidth] = useState(1024);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const renderer = new PptxRenderer();
    rendererRef.current = renderer;
    setError(null);
    setSlideCount(0);

    (async () => {
      try {
        await renderer.load(new Uint8Array(bytes));
        if (cancelled) return;
        setSlideCount(renderer.slideCount);
        setSlide(0);
      } catch (e) {
        if (!cancelled) setError(`演示文稿解析失败：${String(e)}`);
      }
    })();

    return () => {
      cancelled = true;
      renderer.destroy();
      rendererRef.current = null;
    };
  }, [bytes]);

  const draw = useCallback(async () => {
    const renderer = rendererRef.current;
    const canvas = canvasRef.current;
    if (!renderer || !canvas || slideCount === 0) return;
    try {
      await renderer.renderSlide(slide, canvas, width);
    } catch {
      // 单页渲染失败不应当整块崩掉，保留上一帧
    }
  }, [slide, width, slideCount]);

  useEffect(() => {
    void draw();
  }, [draw]);

  if (error) return <div className="preview-state-error">{error}</div>;

  return (
    <div className="preview-pptx">
      <div className="preview-pdf-bar">
        <button
          type="button"
          className="head-action"
          disabled={slide <= 0}
          onClick={() => setSlide((s) => Math.max(0, s - 1))}
          aria-label="上一页"
        >
          <ChevronLeft className="icon-md" />
        </button>
        <span className="preview-pdf-page">
          {slideCount ? `${slide + 1} / ${slideCount}` : "载入中…"}
        </span>
        <button
          type="button"
          className="head-action"
          disabled={slideCount === 0 || slide >= slideCount - 1}
          onClick={() => setSlide((s) => Math.min(slideCount - 1, s + 1))}
          aria-label="下一页"
        >
          <ChevronRight className="icon-md" />
        </button>
        <span className="preview-pdf-spacer" />
        <button
          type="button"
          className="head-action"
          onClick={() => setWidth((w) => Math.max(MIN_WIDTH, w - 160))}
          aria-label="缩小"
        >
          <ZoomOut className="icon-md" />
        </button>
        <button
          type="button"
          className="head-action"
          onClick={() => setWidth((w) => Math.min(MAX_WIDTH, w + 160))}
          aria-label="放大"
        >
          <ZoomIn className="icon-md" />
        </button>
      </div>
      <div className="preview-pptx-stage">
        <canvas ref={canvasRef} />
      </div>
    </div>
  );
}
