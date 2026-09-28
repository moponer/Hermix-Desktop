// 文件预览的类型层：一次判定 → 一种载荷 → 一个渲染器。
//
// 旧实现把「判定 / 加载 / 渲染」三件事揉进同一个组件，且面板与全屏各
// 渲染一遍同样的状态机（两份 switch），新增一种文件类型要改两处，漏一
// 处就是"改了没变化"。这里把三件事拆开：
//   resolveKind() 文件名 → PreviewKind      （纯函数，可单测）
//   loaders[]      PreviewKind → Payload    （副作用隔离在这一层）
//   PreviewSurface Payload → JSX            （面板与全屏共用同一份）

import type { PreviewModel } from "../../lib/fileParser";

/** 预览支持的载荷种类，与文件类型一一对应。 */
export type PreviewKind =
  | "image"
  | "pdf"
  | "markdown"
  | "docx"
  | "pptx"
  | "office"
  | "spreadsheet"
  | "code"
  | "text";

/** 排版视图（officecli / 内嵌 PDF）与文本视图（markitdown 转 Markdown）。 */
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
  | { kind: "docx"; bytes: Uint8Array; fileName: string }
  | { kind: "pptx"; bytes: Uint8Array; fileName: string }
  | { kind: "office"; htmlContent: string; fileName: string }
  | { kind: "spreadsheet"; model: PreviewModel; fileName: string }
  | { kind: "code"; content: string; language: string | null }
  | { kind: "text"; content: string };

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
};

export type LoadOutcome =
  | { ok: true; payload: PreviewPayload; resolvedPath: string | null }
  | { ok: false; message: string };

export type PreviewLoader = (req: LoadRequest) => Promise<LoadOutcome>;

/** 只有排版视图与文本视图之争的类型才需要视图切换按钮。 */
export function supportsTextView(kind: PreviewKind): boolean {
  return kind === "office" || kind === "pdf" || kind === "docx" || kind === "pptx";
}
