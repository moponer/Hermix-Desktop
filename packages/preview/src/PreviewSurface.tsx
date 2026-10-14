// 唯一的渲染入口：一种载荷 → 一块 JSX。
//
// 面板与全屏共用这一个组件。旧实现把同样的 switch 抄了两遍（面板一份、
// 全屏一份），新增类型漏改一处就表现为"改了没变化"——这是那份重复的
// 直接后果，所以渲染必须只有一处。

import { Suspense, lazy } from "react";
import { Loader2 } from "lucide-react";
import { CodeBlock } from "./lib/CodeBlock";
import { SpreadsheetView } from "./viewers/SpreadsheetView";
import { ImageView } from "./viewers/ImageView";
import { MarkdownView } from "./viewers/MarkdownView";
import { HtmlView } from "./viewers/HtmlView";
import { DocCanvasView } from "./viewers/DocCanvasView";
import type { FileSource } from "./lib/fileSource";
import type { PreviewPayload } from "./types";

// 三个转换引擎（pdf.js / docx-preview / pptx-browser）加起来超过 1.5 MB，
// 按需加载——打开纯文本或 Markdown 时不该为它们付下载和解析成本。
const PdfView = lazy(() =>
  import("./viewers/PdfView").then((m) => ({ default: m.PdfView })),
);
const DocxView = lazy(() =>
  import("./viewers/DocxView").then((m) => ({ default: m.DocxView })),
);
const PptxView = lazy(() =>
  import("./viewers/PptxView").then((m) => ({ default: m.PptxView })),
);

/** 引擎加载中的占位，样式与"读取文件"一致，避免闪烁两次不同的 loading。 */
function EngineFallback() {
  return (
    <div className="preview-state-center">
      <Loader2 className="icon-md animate-spin" />
      正在载入渲染引擎…
    </div>
  );
}

/** 需要懒加载引擎的载荷走 Suspense，其余直接渲染。 */
function LazyEngine({ children }: { children: React.ReactNode }) {
  return <Suspense fallback={<EngineFallback />}>{children}</Suspense>;
}

/**
 * Markdown 内嵌的代码块与 html raw 资源需要从磁盘读 —— 由 caller 提供
 * 一份 FileSource，让包不绑死 artifact reader。htmlViewer 的相对资源经
 * 桥脚本转系统浏览器，不需要这层。
 *
 * 视觉态参数 `isDark` / `wrap` 由调用方持有，渲染层不直接耦合样式 —— 但
 * 留这两个 prop 是为了 CodeView 风格切换（CodeView 实例由桌面面板持有，
 * viewer 走共享 CodeBlock）。viewer 当前不用 CodeView，但保留签名便于两
 * 端对齐。
 *
 * 两个可选注入口是给 desktop 的差异点（依赖倒置，包不反向依赖应用层）：
 *   - `renderCode`：代码载荷的渲染器。desktop 注入自己的 CodeView（带
 *     Ln/Col 光标追踪与换行开关）；缺省用包内 CodeBlock。
 *   - `onOpenImage`：图片点击放大。desktop 注入灯箱（openImageViewer）；
 *     缺省无放大行为。
 */
export function PreviewSurface({
  payload,
  source,
  isDark,
  fileName,
  resolvedPath,
  zoom,
  onOpenExternalUrl,
  renderCode,
  onOpenImage,
}: {
  payload: PreviewPayload;
  source: FileSource;
  isDark: boolean;
  fileName: string;
  resolvedPath: string | null;
  zoom: number;
  /** HtmlView 桥脚本点击 / 「在浏览器打开」按钮的统一出口 —— desktop
   *  走 Tauri `openExternalUrl`，viewer 走 `openWithSystem` / 浏览器。 */
  onOpenExternalUrl?: (target: string) => void;
  /** 代码载荷渲染器（desktop 注入 CodeView；缺省包内 CodeBlock）。 */
  renderCode?: (content: string, language: string | null) => React.ReactNode;
  /** 图片点击放大（desktop 注入灯箱；缺省不可点）。 */
  onOpenImage?: (bytes: Uint8Array, mime: string, fileName: string) => void;
}) {
  switch (payload.kind) {
    case "image":
      return (
        <ImageView
          bytes={payload.bytes}
          mime={payload.mime}
          onOpenViewer={
            onOpenImage ? () => onOpenImage(payload.bytes, payload.mime, fileName) : undefined
          }
        />
      );
    case "pdf":
      return (
        <LazyEngine>
          <PdfView bytes={payload.bytes} />
        </LazyEngine>
      );
    case "docx":
      return (
        <LazyEngine>
          <DocxView bytes={payload.bytes} zoom={zoom} />
        </LazyEngine>
      );
    case "pptx":
      return (
        <LazyEngine>
          <PptxView bytes={payload.bytes} />
        </LazyEngine>
      );
    case "markdown":
      return (
        <MarkdownView
          content={payload.content}
          source={source}
          isDark={isDark}
          resolvedPath={resolvedPath}
          zoom={zoom}
        />
      );
    case "html":
      return (
        <HtmlView
          content={payload.content}
          resolvedPath={resolvedPath}
          zoom={zoom}
          fileName={fileName}
          onOpenExternalUrl={onOpenExternalUrl}
        />
      );
    case "canvas":
      // 文档画布：内容经 docCanvasStore 流式直达 viewer，不走 payload。
      return <DocCanvasView fileName={payload.fileName} />;
    case "spreadsheet":
      return <SpreadsheetView model={payload.model} fileName={payload.fileName} zoom={zoom} />;
    case "code":
      if (renderCode) return <>{renderCode(payload.content, payload.language)}</>;
      return <CodeBlock language={payload.language ?? "text"} code={payload.content} isDark={isDark} />;
    case "text":
      return <pre className="preview-plain-text">{payload.content}</pre>;
  }
}
