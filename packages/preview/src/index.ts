// 共享包入口 —— 只导出渲染层 + 模型层；不含 Tauri 桥或桌面应用状态机。
//
// 一次 `loadPreview` 调用 = 一份 `FileSource`（desktop 走 artifact reader，
// viewer 走 Tauri 命令）+ 一次 `LoadRequest`，返回 `LoadOutcome`。
// 上层只需要把 `payload` 喂给 `PreviewSurface`，剩下的 7 个 viewer
// 各自解决自己的引擎 / DOM / 状态。

export type {
  PreviewKind,
  ViewMode,
  PreviewPayload,
  PreviewState,
  LoadRequest,
  LoadOutcome,
  PreviewLoader,
} from "./types";
export { supportsTextView, supportsZoom } from "./types";

export { resolveKind, applyViewMode, type ArtifactKindHint } from "./resolveKind";

export { buildLoaders, loadPreview } from "./loaders";
export type { FileSource } from "./loaders";

export type {
  ReadTextResult,
  ReadBytesResult,
  ReadMarkdownResult,
  DirEntryLike,
  FileStatLike,
  DriveRootLike,
  DirListResult,
  FileSystemOps,
} from "./lib/fileSource";

export {
  IMAGE_BYTE_LIMIT,
  IMAGE_EXT_MIME,
  isImageExt,
  isMarkdownExt,
  isPdfExt,
  isHtmlExt,
  detectLanguage,
  needsFrontendParse,
  officeExtGroup,
  type OfficeExtGroup,
  FRONTEND_PARSE_EXTS,
} from "./lib/previewUtils";

export { PreviewSurface } from "./PreviewSurface";
export { CodeBlock } from "./lib/CodeBlock";
export { hardenSoftLineBreaks } from "./lib/softBreaks";

export { MarkdownView } from "./viewers/MarkdownView";
export { HtmlView } from "./viewers/HtmlView";
export { DocCanvasView } from "./viewers/DocCanvasView";
export { DocxView } from "./viewers/DocxView";
export { SpreadsheetView } from "./viewers/SpreadsheetView";
export { ImageView } from "./viewers/ImageView";
export { PdfView } from "./viewers/PdfView";
export { PptxView } from "./viewers/PptxView";

export {
  docCanvasStore,
  DocCanvasStore,
  CANVAS_OPEN_TAG,
  CANVAS_CLOSE_TAG,
  type CanvasMeta,
  type CanvasPageGeometry,
} from "./lib/docCanvasStore";

export {
  parseRange,
  parseXlsx,
  parseXls,
  parseDelimited,
  sheetToCsv,
  styleToCss,
  colWidthPx,
  rowHeightPx,
  type ParsedWorkbook,
  type SheetModel,
  type CellModel,
  type CellStyle,
  type ColorLike,
  type SheetMerge,
} from "./lib/workbook";

export {
  htmlToXlsxModel,
  htmlToXlsxBytes,
  htmlToDocxBlocks,
  htmlToDocxBytes,
  type XlsxModel,
  type XlsxSheetModel,
  type DocxBlock,
} from "./lib/exporters";

export { stripFrontmatter, extractHeadings } from "./lib/markdownDoc";
export { decodeTextBytes, textResultFromBytes, type DecodedText } from "./lib/textDecode";
export { colName, resolveRelative, dirnameOf, isExternalHref } from "./lib/localAssets";
