// 文件预览的类型层：一次判定 → 一种载荷 → 一个渲染器。
//
// 旧实现把「判定 / 加载 / 渲染」三件事揉进同一个组件，且面板与全屏各
// 渲染一遍同样的状态机（两份 switch），新增一种文件类型要改两处，漏一
// 处就是"改了没变化"。这里把三件事拆开：
//   resolveKind() 文件名 → PreviewKind      （纯函数，可单测）
//   loaders[]      PreviewKind → Payload    （副作用隔离在这一层）
//   PreviewSurface Payload → JSX            （面板与全屏共用同一份）

import type { ParsedWorkbook } from "./lib/workbook";

/** 预览支持的载荷种类，与文件类型一一对应。 */
export type PreviewKind =
  | "image"
  | "pdf"
  | "markdown"
  | "html"
  | "docx"
  | "pptx"
  | "spreadsheet"
  | "code"
  | "text"
  | "canvas";

/**
 * 视图模式：排版视图（按源文件的版面渲染）与文本视图（退化成可搜索、可
 * 复制的源码/Markdown）。PDF 切文本视图要过后端 markitdown 转 Markdown；
 * HTML 无需转换，源码就是源码。
 */
export type ViewMode = "layout" | "text";

/**
 * 载荷统一携带原始字节：各渲染引擎的输入形态不同（pdf.js 吃 URL、
 * docx-preview 吃 Blob/ArrayBuffer、pptx-browser 吃 Uint8Array），
 * 与其在加载层猜，不如把字节交给渲染层按需转换。
 */
export type PreviewPayload =
  | { kind: "image"; bytes: Uint8Array; mime: string; size: number }
  | { kind: "pdf"; bytes: Uint8Array; fileName: string }
  | { kind: "markdown"; content: string; fileName: string; truncated: boolean }
  | { kind: "html"; content: string; fileName: string; resolvedPath: string | null }
  | { kind: "docx"; bytes: Uint8Array; fileName: string }
  | { kind: "pptx"; bytes: Uint8Array; fileName: string }
  | { kind: "spreadsheet"; model: ParsedWorkbook; fileName: string }
  | { kind: "code"; content: string; language: string | null }
  | { kind: "text"; content: string }
  /** 文档画布：内容不走 payload（流式块经 docCanvasStore 直达 viewer），
   *  载荷只携带壳层需要的元信息。 */
  | { kind: "canvas"; fileName: string };

export type PreviewState =
  | { status: "idle" }
  | { status: "loading"; kind: PreviewKind }
  | { status: "ready"; kind: PreviewKind; payload: PreviewPayload; resolvedPath: string | null }
  | { status: "error"; message: string };

/** 加载器的输入。刻意保持最小：路径 + 根 + 视图模式。 */
export type LoadRequest = {
  name: string;
  path: string;
  workspaceRoot?: string | null;
  viewMode: ViewMode;
  /** 已被解析的 PreviewKind —— 调度按它走，必要时由 caller 用
   * `applyViewMode(kind, viewMode)` 后传入。 */
  kind: PreviewKind;
};

export type LoadOutcome =
  | { ok: true; payload: PreviewPayload; resolvedPath: string | null }
  | { ok: false; message: string };

export type PreviewLoader = (req: LoadRequest) => Promise<LoadOutcome>;

/** 需要「排版 / 文本」切换按钮的种类。 */
export function supportsTextView(kind: PreviewKind): boolean {
  return kind === "pdf" || kind === "docx" || kind === "pptx" || kind === "html";
}

/**
 * 需要「缩放 / 适应宽度」的种类。PDF 与 PPTX 是画布，缩放由引擎自己的
 * 像素倍率管（见各自 viewer），这里的按钮是给流式排版内容用的。
 */
export function supportsZoom(kind: PreviewKind): boolean {
  return kind === "markdown" || kind === "docx" || kind === "spreadsheet" || kind === "html";
}
