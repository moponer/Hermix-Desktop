// 共享预览管线在 viewer 侧的 FileSource adapter：把 viewer 自己 Rust
// 后端的 6 个命令（viewer_read_file / list_dir / stat / parent /
// drives / search_dir / reveal / open_with_system）包装成统一的
// readText / readBytes / readMarkdown / fs 形状。
//
// 设计目标：
//   - 一份实现覆盖两种"读"语义：text 解 utf-8 得 String；二进制透传
//     Uint8Array；View=markitdown 走 view 自身（实际 Rust 侧没单独命令）——
//     现阶段 viewer 不集成 markitdown，退化到 readText+kind:text。
//   - 错误一律收敛成 `{ ok: false, message }`，不让底层 invoke 异常直接
//     泄到渲染层。

import { invoke } from "@tauri-apps/api/core";
import type {
  DirEntryLike,
  DriveRootLike,
  FileSource,
  FileStatLike,
  ReadBytesResult,
  ReadMarkdownResult,
  ReadTextResult,
} from "@hermix/preview";

// Tauri 侧 viewer_read_file 返回这种 discriminated union。
type RustFileContent =
  | { kind: "text"; name: string; variant: string; content: string }
  | { kind: "image"; name: string; mime: string; data_base64: string }
  | { kind: "pdf"; name: string; data_base64: string }
  | { kind: "bytes"; name: string; extension: string; data_base64: string }
  | { kind: "unsupported"; name: string; extension: string; reason: string };

/** base64 → Uint8Array（无第三方依赖：charCodeAt 单字节累加）。 */
function decodeBase64(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

/** base64 字符数 → 解码后字节数（够判大小，不必为拿 size 解一遍全量）。 */
function base64ByteLength(b64: string): number {
  const padding = b64.endsWith("==") ? 2 : b64.endsWith("=") ? 1 : 0;
  return Math.max(0, Math.floor((b64.length * 3) / 4) - padding);
}

function asReadText(c: RustFileContent, path: string): ReadTextResult {
  if (c.kind === "text") {
    return { ok: true, content: c.content, resolvedPath: path };
  }
  if (c.kind === "image" || c.kind === "pdf" || c.kind === "bytes") {
    // 只算不解：调用方（markdownLoader / textLoader / htmlLoader）见到
    // isBinary 就直接退出了，没必要为拿 size 把 50MB 的 base64 全解一遍。
    return { ok: true, isBinary: true, size: base64ByteLength(c.data_base64), resolvedPath: path };
  }
  return { ok: false, message: `不支持的类型（${c.extension ?? c.kind}）：${c.reason}` };
}

function asReadBytes(c: RustFileContent, path: string): ReadBytesResult {
  if (c.kind === "image" || c.kind === "pdf" || c.kind === "bytes") {
    const bytes = decodeBase64(c.data_base64);
    return { ok: true, bytes, size: bytes.byteLength, resolvedPath: path };
  }
  if (c.kind === "text") {
    const u8 = new TextEncoder().encode(c.content);
    return { ok: true, bytes: u8, size: u8.byteLength, resolvedPath: path };
  }
  return { ok: false, message: `不支持的类型（${c.extension ?? c.kind}）：${c.reason}` };
}

/** 读单个文件并按类型分流——三条 read 共用的唯一 invoke 入口。 */
async function readFile(path: string): Promise<
  { ok: true; content: RustFileContent } | { ok: false; message: string }
> {
  try {
    return { ok: true, content: await invoke<RustFileContent>("viewer_read_file", { path }) };
  } catch (e) {
    return { ok: false, message: String(e instanceof Error ? e.message : e) };
  }
}

export const viewerFileSource: FileSource = {
  async readText(path: string): Promise<ReadTextResult> {
    const r = await readFile(path);
    return r.ok ? asReadText(r.content, path) : r;
  },

  async readBytes(path: string): Promise<ReadBytesResult> {
    const r = await readFile(path);
    return r.ok ? asReadBytes(r.content, path) : r;
  },

  async readMarkdown(path: string): Promise<ReadMarkdownResult> {
    // viewer 自己没有 markitdown sidecar：退化到 readText + markdown 视角。
    // 走共享的 readFile 而非 `this.readText` —— 解构方法后 `this` 会丢。
    const r = await readFile(path);
    if (!r.ok) return r;
    const t = asReadText(r.content, path);
    if (!t.ok) return { ok: false, message: t.message };
    if ("isBinary" in t && t.isBinary) {
      return { ok: false, message: "二进制文件无法渲染为 Markdown" };
    }
    if ("isEmpty" in t && t.isEmpty) {
      return { ok: true, markdown: "", fileName: leafName(path), truncated: false, resolvedPath: path };
    }
    if ("content" in t) {
      return {
        ok: true,
        markdown: t.content,
        fileName: leafName(path),
        truncated: false,
        resolvedPath: path,
      };
    }
    return { ok: false, message: "读取失败" };
  },

  fs: {
    async listDir(path: string, showHidden: boolean) {
      try {
        const entries = await invoke<DirEntryLike[]>("viewer_list_dir", { path, showHidden });
        return { ok: true, entries };
      } catch (e) {
        return { ok: false, message: String(e instanceof Error ? e.message : e) };
      }
    },
    async stat(path: string) {
      try {
        return await invoke<FileStatLike>("viewer_stat", { path });
      } catch (e) {
        return {
          path,
          exists: false,
          isDir: false,
          isFile: false,
          size: 0,
          mtime: null,
          hidden: false,
        };
      }
    },
    async parent(path: string) {
      try {
        return (await invoke<string | null>("viewer_parent", { path })) ?? null;
      } catch {
        return null;
      }
    },
    async drives() {
      try {
        return await invoke<DriveRootLike[]>("viewer_drives");
      } catch {
        return [];
      }
    },
    async search(path: string, query: string, showHidden: boolean, max: number) {
      try {
        const entries = await invoke<DirEntryLike[]>("viewer_search_dir", {
          path,
          query,
          showHidden,
          max,
        });
        return { ok: true, entries };
      } catch (e) {
        return { ok: false, message: String(e instanceof Error ? e.message : e) };
      }
    },
  },

  async revealInExplorer(path: string) {
    try {
      await invoke("viewer_reveal_in_explorer", { path });
      return { ok: true };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
  },

  async openWithSystem(path: string) {
    try {
      await invoke("viewer_open_with_system", { path });
      return { ok: true };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
  },
};

function leafName(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}
