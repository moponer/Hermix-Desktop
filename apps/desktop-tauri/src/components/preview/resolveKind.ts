// 文件名 → 预览种类。纯函数，不碰 IO，可单测。
//
// 旧实现是一条 if-else 链上串了五个独立判定（isImageExt / isPdfExt /
// isMarkdownExt / needsFrontendParse / kind === "office"），顺序耦合：
// 谁排在前面谁赢，加一种类型就得想清楚插在哪。这里改成单一分派表，
// 判定顺序写死在一处，返回值是唯一的 PreviewKind。

import type { ArtifactKind } from "../../utils/artifacts";
import {
  detectLanguage,
  isImageExt,
  isMarkdownExt,
  isPdfExt,
  needsFrontendParse,
  officeExtGroup,
} from "../rightSidebarUtils";
import type { PreviewKind, ViewMode } from "./types";

/** 不经过任何 IO，仅按扩展名与产物类型判定。 */
export function resolveKind(name: string, artifactKind?: ArtifactKind): PreviewKind {
  if (isMarkdownExt(name)) return "markdown";
  if (isImageExt(name)) return "image";
  if (isPdfExt(name)) return "pdf";
  // 表格类由前端自己解析（SheetJS），不走任何转换引擎
  if (needsFrontendParse(name)) return "spreadsheet";

  // Office 按「能否纯前端渲染」分流：OOXML 走浏览器内引擎，
  // 老二进制格式（doc/xls/ppt）只能交给 officecli 兜底。
  const group = officeExtGroup(name);
  if (group === "docx" || group === "pptx") return group;
  if (group === "legacy" || artifactKind === "office") return "office";

  if (detectLanguage(name)) return "code";
  return "text";
}

/**
 * 应用视图模式。排版视图不可读（不可搜索/不可复制）时，用户可切到
 * 文本视图——对 Office 与 PDF 走 markitdown 转成 Markdown。
 * 这是分派的一部分，不是渲染时的补丁。
 */
export function applyViewMode(kind: PreviewKind, viewMode: ViewMode): PreviewKind {
  if (viewMode === "text" && (kind === "office" || kind === "pdf")) return "markdown";
  return kind;
}
