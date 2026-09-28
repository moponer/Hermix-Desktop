// DOCX 渲染：docx-preview 直接在浏览器里解析 OOXML 并生成 HTML。
//
// 之前 docx 要走 officecli（32 MB 外部 sidecar）转成 HTML——冷启动要
// 拉起进程、写临时文件，几秒才出画面，且引擎缺失就整块白屏。纯前端
// 解析后这些成本全部消失。

import { useEffect, useRef, useState } from "react";
import { renderAsync } from "docx-preview";

export function DocxView({ bytes }: { bytes: Uint8Array }) {
  const bodyRef = useRef<HTMLDivElement | null>(null);
  const styleRef = useRef<HTMLDivElement | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const body = bodyRef.current;
    if (!body) return;
    body.innerHTML = "";
    setError(null);

    // docx-preview 内部用 JSZip 读 ArrayBuffer，且会消费它；传副本避免
    // 影响 payload 里的原始字节。
    const data = new Uint8Array(bytes).buffer;
    renderAsync(data, body, styleRef.current ?? undefined, {
      inWrapper: false,
      ignoreWidth: true,
      ignoreHeight: true,
      // 页眉页脚不渲染，避免每页重复内容干扰阅读
      renderHeaders: false,
      renderFooters: false,
    }).catch((e: unknown) => {
      setError(`Word 文档解析失败：${String(e)}`);
    });
  }, [bytes]);

  if (error) return <div className="preview-state-error">{error}</div>;

  return (
    <div className="preview-docx">
      <div ref={styleRef} />
      <div ref={bodyRef} className="preview-docx-body" />
    </div>
  );
}
