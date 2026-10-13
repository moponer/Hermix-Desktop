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

import { useEffect, useMemo, useRef, useState } from "react";
import { renderAsync } from "docx-preview";

export function DocxView({ bytes, zoom }: { bytes: Uint8Array; zoom: number }) {
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const styleRef = useRef<HTMLDivElement | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pages, setPages] = useState(0);

  useEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    body.innerHTML = "";
    setError(null);
    setPages(0);

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
      .then(() => setPages(body.querySelectorAll("section.docx").length))
      .catch((e: unknown) => {
        setError(`Word 文档解析失败：${String(e)}`);
      });
  }, [bytes]);

  // 缩放整叠页面而不是改字号：字号一改纸张尺寸就不对，页边距和分页都漂。
  const zoomStyle = useMemo(() => ({ zoom: zoom / 100 }), [zoom]);

  if (error) return <div className="preview-state-error">{error}</div>;

  return (
    <div className="docx-root">
      <div className="preview-md-scroll" role="status">
        <span className="preview-md-meta">{pages ? `${pages} 页` : "解析中…"}</span>
      </div>
      <div className="docx-scroll">
        <div className="docx-shell" style={zoomStyle}>
          <div ref={styleRef} />
          <div ref={bodyRef} />
        </div>
      </div>
    </div>
  );
}
