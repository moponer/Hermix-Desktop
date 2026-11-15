// 桌面侧 FileSource 适配层：把 artifactReader（产物根白名单 + 祖先回溯
// 重试链）包装成 @hermix/preview 的 FileSource 形状。
//
// workspaceRoot 是"每次预览会变"的上下文，而包的 FileSource 方法签名只
// 收 path —— 所以在构造时闭包绑定：调用方按 workspaceRoot memo 一份
// source，传给 buildLoaders / PreviewSurface。
//
// openWithSystem 接 desktopBridge 的 openExternalUrl：包内 MarkdownView
// 的站外链接统一走 source.openWithSystem（原桌面实现直呼 openExternalUrl，
// 行为一致）。

import type {
  FileSource,
  ReadBytesResult,
  ReadMarkdownResult,
  ReadTextResult,
} from "@hermix/preview";
import { openExternalUrl } from "../desktopBridge";
import {
  tryConvertDocumentToMarkdown,
  tryReadArtifact,
  tryReadArtifactBytes,
} from "../utils/artifactReader";

export function desktopFileSource(workspaceRoot?: string | null): FileSource {
  return {
    async readText(path: string): Promise<ReadTextResult> {
      const res = await tryReadArtifact(path, workspaceRoot);
      switch (res.kind) {
        case "text":
          return { ok: true, content: res.content, resolvedPath: res.resolvedPath };
        case "binary":
          return { ok: true, isBinary: true, size: res.size, resolvedPath: res.resolvedPath };
        case "empty":
          return { ok: true, isEmpty: true, resolvedPath: res.resolvedPath };
        default:
          return { ok: false, message: res.message };
      }
    },

    async readBytes(path: string): Promise<ReadBytesResult> {
      const res = await tryReadArtifactBytes(path, workspaceRoot);
      if (res.kind === "error") return { ok: false, message: res.message };
      return {
        ok: true,
        bytes: res.bytes,
        size: res.size,
        resolvedPath: res.resolvedPath,
      };
    },

    async readMarkdown(path: string): Promise<ReadMarkdownResult> {
      const md = await tryConvertDocumentToMarkdown(path, workspaceRoot);
      if ("error" in md) return { ok: false, message: md.error };
      return {
        ok: true,
        markdown: md.markdown,
        fileName: md.fileName,
        truncated: md.truncated,
        resolvedPath: md.resolvedPath,
      };
    },

    async openWithSystem(path: string) {
      try {
        await openExternalUrl(path);
        return { ok: true };
      } catch (e) {
        return { ok: false, message: String(e) };
      }
    },
  };
}
