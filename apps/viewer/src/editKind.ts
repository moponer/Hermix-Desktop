// 「这个文件能不能编辑」的唯一判定处。
//
// 为什么单独一个文件：右键菜单、菜单栏、快捷键、预览工具条四��入口都要
// 回答同一个问题（当前文件有没有编辑器）。判定散在各处就会出现「菜单里
// 显示能编辑、点进去没有」或反过来。

import { isMediaFile } from "./media/MediaView";
import { isEditableImage } from "./image/imageOps";
import type { PreviewKind } from "@hermix/preview";

/** 编辑器种类。文本类（含代码 / Markdown）走文本编辑器。 */
export type EditorKind = "none" | "image" | "pdf" | "text";

/** 这个文件有没有对应的编辑器。SVG 是矢量，图片内核解不了 → 不给。 */
export function editorFor(kind: PreviewKind, name: string): EditorKind {
  // 媒体走 asset 协议播放，不进文本编辑器。
  if (isMediaFile(name)) return "none";
  if (kind === "image") return isEditableImage(name) ? "image" : "none";
  if (kind === "pdf") return "pdf";
  if (kind === "text" || kind === "code" || kind === "markdown") return "text";
  return "none";
}

/** 编辑按钮的提示文案（同时用作 aria-label）。 */
export const EDITOR_HINT: Record<Exclude<EditorKind, "none">, string> = {
  image: "编辑图片（裁剪 / 调整 / 转换格式）",
  pdf: "编辑 PDF（删页 / 排序 / 旋转 / 插空白页）",
  text: "编辑文本（另存为新文件）",
};

/** 菜单里「编辑」这一项的文案。 */
export function editorLabel(kind: EditorKind): string {
  switch (kind) {
    case "image":
      return "编辑图片…";
    case "pdf":
      return "编辑 PDF…";
    case "text":
      return "编辑文本…";
    default:
      return "编辑…";
  }
}
