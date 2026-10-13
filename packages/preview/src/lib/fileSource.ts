// 抽象文件读取层 —— 让预览管线同时被 desktop（artifact reader + 产
// 物根白名单）和 viewer（直连 Tauri `viewer_read_file`）使用。
//
// 这个接口只关心"读出来"与"列出"，不关心权限模型。Desktop 的实现
// 走 artifactReader（带祖先回溯 + 允许根白名单）；viewer 的实现
// 走自己 Rust 侧的 `viewer_read_file` / `viewer_list_dir`。能力差异
// 由 `exists` / 大小限制等在两端各自处理。
//
// 三类原始读：`readText` 返回文本 / 二进制占位 / 空；`readBytes`
// 返回 Uint8Array；`readMarkdown` 返回 markitdown 转出的 Markdown。
// 大小判定（"这个 markdownLoader 不该读 50MB PDF"）由 loader 自己
// 在 `readBytes` 之上做，加载器是与具体文件类型挂钩的唯一一处。

export type ReadTextResult =
  | { ok: true; content: string; resolvedPath: string | null }
  | { ok: true; isBinary: true; size: number; resolvedPath: string | null }
  | { ok: true; isEmpty: true; resolvedPath: string | null }
  | { ok: false; message: string };

export type ReadBytesResult =
  | { ok: true; bytes: Uint8Array; size: number; resolvedPath: string | null }
  | { ok: false; message: string };

export type ReadMarkdownResult =
  | {
      ok: true;
      markdown: string;
      fileName: string;
      truncated: boolean;
      resolvedPath: string | null;
    }
  | { ok: false; message: string };

/** 文件系统元数据（资源管理器 UI 用）。 */
export interface DirEntryLike {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  /** RFC 3339 字符串或 null。 */
  mtime: string | null;
  extension: string;
  hidden: boolean;
}

export interface FileStatLike {
  path: string;
  exists: boolean;
  isDir: boolean;
  isFile: boolean;
  size: number;
  mtime: string | null;
  hidden: boolean;
}

export interface DriveRootLike {
  letter: string;
  path: string;
  label: string;
}

export type DirListResult =
  | { ok: true; entries: DirEntryLike[] }
  | { ok: false; message: string };

export interface FileSystemOps {
  listDir(path: string, showHidden: boolean): Promise<DirListResult>;
  stat(path: string): Promise<FileStatLike>;
  parent(path: string): Promise<string | null>;
  drives(): Promise<DriveRootLike[]>;
  /** 当前目录下按文件名子串搜索（深度 1）。 */
  search(
    path: string,
    query: string,
    showHidden: boolean,
    max: number,
  ): Promise<DirListResult>;
}

export interface FileSource {
  /** 单文件包 read —— 文本 / 二进制占位 / 空 / 报错。 */
  readText(path: string): Promise<ReadTextResult>;
  /** 原始字节 —— 二进制 / 大文件预览必须。 */
  readBytes(path: string): Promise<ReadBytesResult>;
  /** markitdown 转 Markdown —— Office / PDF 文本视图共用。 */
  readMarkdown(path: string): Promise<ReadMarkdownResult>;
  /** 资源管理器（可选：desktop 没有需要，viewer 有）。 */
  fs?: FileSystemOps;
  /** 系统资源管理器高亮选中 — 可选；desktop 通常不做，viewer 做。 */
  revealInExplorer?(path: string): Promise<{ ok: boolean; message?: string }>;
  /** 用系统默认程序打开 — 可选。 */
  openWithSystem?(path: string): Promise<{ ok: boolean; message?: string }>;
}
