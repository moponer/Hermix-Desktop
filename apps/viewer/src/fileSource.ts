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
import { textResultFromBytes } from "@hermix/preview";
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
  | { kind: "text"; name: string; variant: string; data_base64: string }
  | { kind: "image"; name: string; mime: string; data_base64: string }
  | { kind: "pdf"; name: string; data_base64: string }
  | { kind: "bytes"; name: string; extension: string; data_base64: string }
  | {
      kind: "unsupported";
      name: string;
      extension: string;
      reason: string;
      /** 后端判定这是可执行文件（exe / msi / …）——可展示「立即运行」。 */
      executable: boolean;
    };

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

/**
 * 不支持类型的失败结果。
 *
 * 可执行文件单独措辞：后端已经给了「二进制可执行文件不可预览，点击「立即
 * 运行」执行」，再加「不支持的类型（exe）：」前缀会把重点冲淡；其余类型
 * 沿用「不支持的类型（ext）：原因」。
 */
function unsupportedResult(c: Extract<RustFileContent, { kind: "unsupported" }>): {
  ok: false;
  message: string;
  executable?: boolean;
} {
  if (c.executable) {
    return { ok: false, message: c.reason, executable: true };
  }
  return { ok: false, message: `不支持的类型（${c.extension ?? c.kind}）：${c.reason}` };
}

function asReadText(c: RustFileContent, path: string): ReadTextResult {
  if (c.kind === "text") {
    // Rust 送的是原始字节；编码判定（BOM / UTF-16 / GBK）与二进制嗅探
    // 统一在共享层做，规则只有一份（packages/preview/src/lib/textDecode.ts）。
    return textResultFromBytes(decodeBase64(c.data_base64), path);
  }
  if (c.kind === "image" || c.kind === "pdf" || c.kind === "bytes") {
    // 只算不解：调用方（markdownLoader / textLoader / htmlLoader）见到
    // isBinary 就直接退出了，没必要为拿 size 把 50MB 的 base64 全解一遍。
    return { ok: true, isBinary: true, size: base64ByteLength(c.data_base64), resolvedPath: path };
  }
  return unsupportedResult(c);
}

function asReadBytes(c: RustFileContent, path: string): ReadBytesResult {
  if (c.kind === "image" || c.kind === "pdf" || c.kind === "bytes") {
    const bytes = decodeBase64(c.data_base64);
    return { ok: true, bytes, size: bytes.byteLength, resolvedPath: path };
  }
  if (c.kind === "text") {
    // 文本类的原始字节同样要原样透传：重新 encode 一次会把 UTF-16 / GBK
    // 的内容洗成 UTF-8 乱码。
    const bytes = decodeBase64(c.data_base64);
    return { ok: true, bytes, size: bytes.byteLength, resolvedPath: path };
  }
  return unsupportedResult(c);
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

/**
 * viewer 私有的扩展能力（共享 FileSource 里没有 desktop 用得上的部分）。
 *
 * 不塞进 packages/preview 的 FileSource：这些是资源管理器专属动作
 * （剪贴板里的文件、打开方式探测），desktop 侧用不上，往共享接口里
 * 加可选方法只会让两边都背着一份用不到的类型。
 */
export interface ViewerFileSource extends FileSource {
  /** 用指定方式打开：default / notepad / code / properties / reveal。 */
  openWith?(path: string, app: string): Promise<{ ok: boolean; message?: string }>;
  /** 复制文件/文件夹到剪贴板（CF_HDROP，资源管理器式"复制文件"）。 */
  copyFiles?(
    paths: string[],
  ): Promise<{ ok: boolean; message?: string; count?: number }>;
  /** 从剪贴板粘贴文件到目录（同名项跳过，不覆盖）。 */
  pasteFiles?(
    dir: string,
  ): Promise<{ ok: boolean; message?: string; copied?: string[]; skipped?: string[] }>;
  /** 本机可用的"打开方式"（notepad / code…），用于把没装的置灰。 */
  availableApps?(): Promise<string[]>;
  /**
   * 写 / 读剪贴板文本。
   *
   * 走 Rust 而不是 navigator.clipboard：WebView2 里读剪贴板要
   * clipboard-read 权限，而 WebView2 没有权限弹窗——直接失败。
   */
  copyText?(text: string): Promise<{ ok: boolean; message?: string }>;
  pasteText?(): Promise<{ ok: true; text: string } | { ok: false; message: string }>;
}

export const viewerFileSource: ViewerFileSource = {
  async readText(path: string): Promise<ReadTextResult> {
    const r = await readFile(path);
    return r.ok ? asReadText(r.content, path) : r;
  },

  async readBytes(path: string): Promise<ReadBytesResult> {
    const r = await readFile(path);
    return r.ok ? asReadBytes(r.content, path) : r;
  },

  async readMarkdown(path: string): Promise<ReadMarkdownResult> {
    // 走 markitdown sidecar（它就在本 exe 旁边的 binaries/ 里，与桌面端
    // 共用同一份二进制）。此前这里退化成 readText，于是 PDF 的「文本」视图
    // 一点就报"二进制文件无法渲染为 Markdown"，epub / msg 更是完全打不开。
    try {
      const r = await invoke<{
        markdown: string;
        fileName: string;
        truncated: boolean;
      }>("document_to_markdown", { filePath: path });
      return {
        ok: true,
        markdown: r.markdown,
        fileName: r.fileName || leafName(path),
        truncated: r.truncated,
        resolvedPath: path,
      };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
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

  /**
   * 立即运行可执行文件。确认框在 Rust 侧弹（原生对话框，用户必须亲手
   * 点「运行」）——这里只负责把结果如实传回去：用户取消不是错误。
   */
  async runExecutable(path: string) {
    try {
      await invoke<boolean>("viewer_run_executable", { path });
      return { ok: true };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
  },

  /**
   * 全盘模糊搜索（本地索引，类 Everything）。
   *
   * 索引服务没跑时 Rust 侧会尝试提权拉起（UAC 弹一次）；拉不起来就返回
   * 失败，前端据此退回「当前目录内搜索」——搜索这条路不能因为索引缺失
   * 就整个不通。
   */
  async searchGlobal(query: string, limit: number) {
    try {
      const r = await invoke<{
        entries: DirEntryLike[];
        totalMatches: number;
        returned: number;
        elapsedMs: number;
      }>("viewer_search_global", { query, limit });
      return { ok: true, ...r };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
  },

  /** 用指定方式打开（右键菜单的「打开方式」）。 */
  async openWith(path: string, app: string) {
    try {
      await invoke("viewer_open_with", { path, app });
      return { ok: true };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
  },

  /** 复制文件/文件夹到剪贴板（CF_HDROP）。 */
  async copyFiles(paths: string[]) {
    try {
      const count = await invoke<number>("viewer_copy_files", { paths });
      return { ok: true, count };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
  },

  /** 从剪贴板粘贴文件到目录（同名项跳过，不覆盖）。 */
  async pasteFiles(dir: string) {
    try {
      const r = await invoke<{ copied: string[]; skipped: string[] }>(
        "viewer_paste_files",
        { dir },
      );
      return { ok: true, ...r };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
  },

  /** 本机可用的「打开方式」。探测失败就当只有系统默认程序。 */
  async availableApps() {
    try {
      return await invoke<string[]>("viewer_available_apps");
    } catch {
      return [] as string[];
    }
  },

  /** 写剪贴板文本。 */
  async copyText(text: string) {
    try {
      await invoke("viewer_copy_text", { text });
      return { ok: true };
    } catch (e) {
      return { ok: false, message: String(e instanceof Error ? e.message : e) };
    }
  },

  /** 读剪贴板文本。 */
  async pasteText() {
    try {
      const text = await invoke<string>("viewer_paste_text");
      return { ok: true as const, text };
    } catch (e) {
      return { ok: false as const, message: String(e instanceof Error ? e.message : e) };
    }
  },
};

function leafName(p: string): string {
  return p.split(/[\\/]/).pop() ?? p;
}
