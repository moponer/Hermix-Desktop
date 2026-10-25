// 「这个文件能不能编辑」的唯一判定处。
//
// 为什么单独一个文件：右键菜单、菜单栏、快捷键、预览工具条四��入口都要
// 回答同一个问题（当前文件有没有编辑器）。判定散在各处就会出现「菜单里
// 显示能编辑、点进去没有」或反过来。

import { isMediaFile } from "./media/MediaView";
import { isEditableImage } from "./image/imageOps";
import type { PreviewKind } from "@hermix/preview";

/**
 * 编辑器种类。文本类（含代码 / Markdown）走文本编辑器。
 *
 * **不含 Word / Excel**：那两类不是「按一下编辑才进编辑态」，而是**打开
 * 就是可编辑的文档/表格本体**（见 PreviewHost 的 officeInline 分支）——
 * Office 软件本来就是这样，套一层「编辑模式」反而不像。
 */
export type EditorKind = "none" | "image" | "pdf" | "text";

/** 这个文件有没有对应的编辑器。SVG 是矢量，图片内核解不了 → 不给。 */
export function editorFor(kind: PreviewKind, name: string): EditorKind {
  // 媒体走 asset 协议播放，不进文本编辑器。
  if (isMediaFile(name)) return "none";
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (kind === "image") return isEditableImage(name) ? "image" : "none";
  // PDF 点开只读预览：编辑器（删页/排序/旋转/插页）能力未成熟，先不设
  // 编辑入口——右键菜单、Ctrl+E、工具条按钮随本判定一并消失。能力成熟
  // 后把 "none" 换回 "pdf" 即恢复。
  if (kind === "pdf") return "none";
  // csv/tsv 是纯文本：按文本改最直接（也保得住编码与分隔符）。
  // xlsx/xlsm 走表格编辑器，docx 走文档编辑器，两者都在预览层就地编辑，
  // 不经过这里的「编辑模式」。
  if (kind === "spreadsheet") return ext === "csv" || ext === "tsv" ? "text" : "none";
  if (kind === "text" || kind === "code" || kind === "markdown") return "text";
  return "none";
}

/** 编辑按钮的提示文案（同时用作 aria-label）。 */
export const EDITOR_HINT: Record<Exclude<EditorKind, "none">, string> = {
  image: "编辑图片（裁剪 / 调整 / 转换格式）",
  pdf: "编辑 PDF（删页 / 排序 / 旋转 / 插空白页）",
  text: "编辑文本（另存为新文件）",
};

/**
 * 菜单里「编辑」这一项的文案。
 *
 * 2026-10-23：去掉「…」后缀与冗余词。菜单项是**命令**不是对话框标题，
 * 省略号在 Windows 约定里表示「还会再弹一层」，而这里直接进编辑器；
 * 冗长的「系统资源管理…」这类 7 字标签会把菜单宽度撑到 236px 以上，
 * 反而更难扫。短标签 + 图标是菜单栏的正确密度。
 */
export function editorLabel(kind: EditorKind): string {
  switch (kind) {
    case "image":
      return "编辑图片";
    case "pdf":
      return "编辑 PDF";
    case "text":
      return "编辑文本";
    default:
      return "编辑";
  }
}
