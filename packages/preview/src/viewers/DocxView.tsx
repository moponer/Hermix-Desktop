// DOCX 渲染：docx-preview 在浏览器里解析 OOXML 并生成带版面的 HTML。
//
// 走纯前端解析是为了避开外部进程：拉起 sidecar 要冷启动、写临时文件，
// 几秒才出画面，且引擎缺失就整块白屏。浏览器内解析没有这些成本。
//
// 版面保真全靠选项。docx-preview 的默认值就是"按页面还原"，此前这里逐项
// 把它关掉了（ignoreWidth / ignoreHeight / renderHeaders / renderFooters），
// 于是文档退化成一条没有页宽、没有分页、丢了页眉页脚的连续文本流——
// 用户看到的"Word 格式没还原"就是这一组开关造成的。现在只显式声明需要
// 与默认不同的项，其余交给库。
//
// 滚动形态与 PDF/PPTX 对齐（连续纵向滚动 + 左侧缩略图），但实现不能共用：
// docx-preview 一次吐出整篇 DOM，没有"单页渲染"这回事。这里直接滚它生成的
// 那叠 section.docx，缩略图用缩放克隆（只为侧栏里看得见的那几页做）。

import { useEffect, useMemo, useRef, useState } from "react";
import { renderAsync } from "docx-preview";
import { ChevronLeft, ChevronRight, PanelLeft } from "lucide-react";
import { usePageScroll } from "../lib/usePageScroll";
import { NearSlot, THUMB_WIDTH } from "./DocPager";

/**
 * 一页的缩略图：克隆那节 DOM 再 scale 下去。
 * docx-preview 的"页"是 DOM 里的一节，没有 canvas 可以重画；克隆比截图便宜，
 * 也比整篇再渲染一遍安全（后者会在侧栏里跑第二套排版）。
 */
function DocxThumb({ pageEl, width }: { pageEl: HTMLElement; width: number }) {
  const boxRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const box = boxRef.current;
    if (!box) return;
    const w = pageEl.offsetWidth || 816;
    const h = pageEl.offsetHeight || 1056;
    const clone = pageEl.cloneNode(true) as HTMLElement;
    clone.removeAttribute("id");
    // 克隆体不带原节的排版上下文，宽高写死再整体缩小，避免在侧栏里重排。
    clone.style.width = `${w}px`;
    clone.style.minHeight = `${h}px`;
    clone.style.margin = "0";
    clone.style.boxShadow = "none";
    clone.style.transformOrigin = "top left";
    clone.style.transform = `scale(${width / w})`;
    box.replaceChildren(clone);
  }, [pageEl, width]);

  return <div ref={boxRef} className="docpager-docx-thumb" />;
}

export function DocxView({ bytes, zoom }: { bytes: Uint8Array; zoom: number }) {
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const styleRef = useRef<HTMLDivElement | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [sections, setSections] = useState<HTMLElement[]>([]);
  const [stageEl, setStageEl] = useState<HTMLDivElement | null>(null);
  const [thumbsOpen, setThumbsOpen] = useState(false);
  const thumbsInit = useRef(false);
  const railRef = useRef<HTMLDivElement | null>(null);

  const { current, register, scrollTo } = usePageScroll(stageEl, sections.length);

  useEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    body.innerHTML = "";
    setError(null);
    setSections([]);

    // docx-preview 内部用 JSZip 读 ArrayBuffer，且会消费它；传副本避免
    // 影响 payload 里的原始字节。
    const data = new Uint8Array(bytes).buffer;
    renderAsync(data, body, styleRef.current ?? undefined, {
      // inWrapper=true：生成 .docx-wrapper + <section class="docx"> 的纸张
      // 结构，分页、纸张阴影、页边距才有载体。
      inWrapper: true,
      // ignoreWidth/ignoreHeight=false：按 sectPr 的纸张尺寸出页面。
      // 关掉这两项是此前"宽度没还原"的直接原因。
      ignoreWidth: false,
      ignoreHeight: false,
      // 页眉页脚属于文档内容，必须渲染。
      renderHeaders: true,
      renderFooters: true,
      renderFootnotes: true,
      renderEndnotes: true,
      // 图片走 base64 内联，沙箱之外不依赖任何外部资源。
      useBase64URL: true,
      className: "docx",
    })
      .then(() => {
        setSections(Array.from(body.querySelectorAll<HTMLElement>("section.docx")));
      })
      .catch((e: unknown) => {
        setError(`Word 文档解析失败：${String(e)}`);
      });
  }, [bytes]);

  // 把生成的每节注册给滚动计数（当前页/跳页都靠它量 DOM）。
  useEffect(() => {
    sections.forEach((el, i) => register(i)(el));
  }, [sections, register]);

  // 缩略图侧栏默认值：宽面板给，窄栏（桌面右栏）不给，与 PDF/PPTX 一致。
  useEffect(() => {
    if (thumbsInit.current || !stageEl) return;
    thumbsInit.current = true;
    if (stageEl.clientWidth - 32 >= 520) setThumbsOpen(true);
  }, [stageEl]);

  // 当前页的缩略图跟着滚进可视区（block:"nearest"：已经看得见就不动）。
  useEffect(() => {
    if (!thumbsOpen) return;
    railRef.current?.children[current - 1]?.scrollIntoView({ block: "nearest" });
  }, [current, thumbsOpen]);

  // 缩放整叠页面而不是改字号：字号一改纸张尺寸就不对，页边距和分页都漂。
  const zoomStyle = useMemo(() => ({ zoom: zoom / 100 }), [zoom]);

  if (error) return <div className="preview-state-error">{error}</div>;

  return (
    <div className="docx-root">
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
        <span className="preview-pdf-page">
          {sections.length ? `${current} / ${sections.length}` : "解析中…"}
        </span>
        <button
          type="button"
          className="head-action"
          disabled={sections.length === 0 || current >= sections.length}
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
      </div>

      <div className="docpager-body">
        {thumbsOpen && sections.length > 0 && (
          <div className="docpager-thumbs" role="group" aria-label="页面缩略图" ref={railRef}>
            {sections.map((el, i) => {
              const n = i + 1;
              const ratio = el.offsetWidth > 0 ? el.offsetHeight / el.offsetWidth : 1.414;
              return (
                <button
                  key={n}
                  type="button"
                  className={n === current ? "docpager-thumb is-active" : "docpager-thumb"}
                  onClick={() => scrollTo(n)}
                  aria-label={`跳到第 ${n} 页`}
                  aria-current={n === current ? "page" : undefined}
                >
                  <NearSlot width={THUMB_WIDTH} height={Math.round(THUMB_WIDTH * ratio)}>
                    <DocxThumb pageEl={el} width={THUMB_WIDTH} />
                  </NearSlot>
                  <span className="docpager-thumb-no">{n}</span>
                </button>
              );
            })}
          </div>
        )}

        <div className="docx-scroll" ref={setStageEl}>
          <div className="docx-shell" style={zoomStyle}>
            <div ref={styleRef} />
            <div ref={bodyRef} />
          </div>
        </div>
      </div>
    </div>
  );
}
