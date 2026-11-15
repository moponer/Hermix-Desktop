// 文件名 → 预览种类。纯函数，不碰 IO，可单测。
//
// 旧实现是一条 if-else 链上串了五个独立判定（isImageExt / isPdfExt /
// isMarkdownExt / needsFrontendParse / kind === "office"），顺序耦合：
// 谁排在前面谁赢，加一种类型就得想清楚插在哪。这里改成单一分派表，
// 判定顺序写死在一处，返回值是唯一的 PreviewKind。

import {
  detectLanguage,
  isHtmlExt,
  isImageExt,
  isMarkdownExt,
  isPdfExt,
  needsFrontendParse,
  officeExtGroup,
} from "./lib/previewUtils";
import type { PreviewKind, ViewMode } from "./types";

export interface ArtifactKindHint {
  // 当后端为旧二进制 Office (doc/ppt) 打了 office 标签，可走 Markdown 退路。
  legacy?: boolean;
}

/** 不经过任何 IO，仅按扩展名与产物类型判定。 */
export function resolveKind(name: string, artifact?: ArtifactKindHint): PreviewKind {
  if (isMarkdownExt(name)) return "markdown";
  // html 必须排在 detectLanguage 前面：否则 .html 会被语言表认成 "markup"
  // 直接进代码视图 —— 页面根本渲染不出来，这正是用户报的"只能看代码"。
  if (isHtmlExt(name)) return "html";
  if (isImageExt(name)) return "image";
  if (isPdfExt(name)) return "pdf";
  // 表格类由前端自己解析（xlsx 走 ExcelJS、xls 走 SheetJS、csv 走 Papa）
  if (needsFrontendParse(name)) return "spreadsheet";

  // Office 按"能否纯前端渲染"分流：OOXML 由浏览器内引擎还原版面；
  // 老二进制格式（doc / ppt）没有任何前端解析器，也不再有版式渲染器，
  // 直接落 markitdown 的文本表示。
  const group = officeExtGroup(name);
  if (group === "docx" || group === "pptx") return group;
  if (group === "legacy" || artifact?.legacy) return "markdown";

  if (detectLanguage(name)) return "code";
  return "text";
}

/**
 * 应用视图模式。排版视图不可读（不可搜索/不可复制）时，用户可切到
 * 文本视图——PDF 走 markitdown 转成 Markdown，HTML 直接读源码。
 * 这是分派的一部分，不是渲染时的补丁。
 */
export function applyViewMode(kind: PreviewKind, viewMode: ViewMode): PreviewKind {
  if (viewMode !== "text") return kind;
  if (kind === "html") return "code";
  if (kind === "pdf") return "markdown";
  return kind;
}
