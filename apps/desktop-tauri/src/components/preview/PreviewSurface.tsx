// 唯一的渲染入口：一种载荷 → 一块 JSX。
//
// 面板与全屏共用这一个组件。旧实现把同样的 switch 抄了两遍（面板一份、
// 全屏一份），新增类型漏改一处就表现为"改了没变化"——这是那份重复的
// 直接后果，所以渲染必须只有一处。

import { Suspense, lazy } from "react";
import { Loader2 } from "lucide-react";
import { CodeView } from "../CodeView";
import { SpreadsheetView } from "./viewers/SpreadsheetView";
import { ImageView } from "./viewers/ImageView";
import { MarkdownView } from "./viewers/MarkdownView";
import { HtmlView } from "./viewers/HtmlView";
import { openImageViewer } from "../imageViewer";
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

/** 代码视图的行号由面板持有（状态栏要显示 Ln/Col），这里只做受控渲染。 */
function CodeSurface({
  content,
  language,
  wrap,
  isDark,
  cursor,
  onCursorChange,
}: {
  content: string;
  language: string | null;
  wrap: boolean;
  isDark: boolean;
  cursor: { line: number; col: number };
  onCursorChange: (c: { line: number; col: number }) => void;
}) {
  if (!language) {
    return <pre className="preview-plain-text">{content}</pre>;
  }
  return (
    <CodeView
      source={content}
      language={language}
      wrap={wrap}
      isDark={isDark}
      activeLine={cursor.line}
      onLineClick={(line) => onCursorChange({ line, col: 1 })}
    />
  );
}

export function PreviewSurface({
  payload,
  wrap,
  isDark,
  fileName,
  resolvedPath,
  zoom,
  cursor,
  onCursorChange,
}: {
  payload: PreviewPayload;
  wrap: boolean;
  isDark: boolean;
  fileName: string;
  resolvedPath: string | null;
  zoom: number;
  cursor: { line: number; col: number };
  onCursorChange: (c: { line: number; col: number }) => void;
}) {
  switch (payload.kind) {
    case "image":
      return (
        <ImageView
          bytes={payload.bytes}
          mime={payload.mime}
          onOpenViewer={() =>
            openImageViewer({
              src: URL.createObjectURL(new Blob([payload.bytes], { type: payload.mime })),
              name: fileName,
            })
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
        <MarkdownView content={payload.content} isDark={isDark} resolvedPath={resolvedPath} zoom={zoom} />
      );
    case "html":
      return <HtmlView content={payload.content} resolvedPath={resolvedPath} zoom={zoom} />;
    case "spreadsheet":
      return <SpreadsheetView model={payload.model} fileName={payload.fileName} zoom={zoom} />;
    case "code":
      return (
        <CodeSurface
          content={payload.content}
          language={payload.language}
          wrap={wrap}
          isDark={isDark}
          cursor={cursor}
          onCursorChange={onCursorChange}
        />
      );
    case "text":
      return <pre className="preview-plain-text">{payload.content}</pre>;
  }
}
