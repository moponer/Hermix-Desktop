// 工作台存储层的前端封装。
// 工作台在宿主内运行、无需打包，因此不走 sidecar HTTP，直接走 Tauri IPC。
//
// 例外：会话归属（`sessions.workbench_id`）走 HTTP —— 会话表由 sidecar
// 持有，桌面端直接开库会和它抢写锁，只能经由后端改。
import { invoke } from "@tauri-apps/api/core";
import { getSessions, updateSessionMetadata } from "../api/rest";

export interface WorkbenchManifest {
  id: string;
  name: string;
  /** 绑定的持久会话 id；右侧对话栏与 skill 常驻都靠它。 */
  session_id: string | null;
  created_at: number;
  updated_at: number;
  /** 依赖白名单内的第三方库名，宿主据此注入 importmap。 */
  deps: string[];
  /** 主工作台标记。工作台页面默认显示主台，全局至多一个为 true。 */
  is_primary: boolean;
}

export const workbenchCreate = (name: string) =>
  invoke<WorkbenchManifest>("workbench_create", { name });

export const workbenchList = () => invoke<WorkbenchManifest[]>("workbench_list");

/** 设为主工作台，其余自动清掉标记。 */
export const workbenchSetPrimary = (id: string) =>
  invoke<void>("workbench_set_primary", { id });

/** 软删工作台：目录移入 `_trash/` 可找回。返回回收站路径。 */
export const workbenchDelete = (id: string) =>
  invoke<string>("workbench_delete", { id });

export const workbenchRead = (id: string, path: string) =>
  invoke<string>("workbench_read", { id, path });

export const workbenchWrite = (id: string, path: string, content: string) =>
  invoke<void>("workbench_write", { id, path, content });

export const workbenchFiles = (id: string) => invoke<string[]>("workbench_files", { id });

export const workbenchBindSession = (id: string, sessionId: string) =>
  invoke<void>("workbench_bind_session", { id, sessionId });

/**
 * 确保会话已绑定，并把它划归本工作台的对话空间。
 * `sessionId` 由调用方（前端）准备 —— 会话创建走 HTTP，Rust 侧拿不到。
 *
 * 绑一次就够，之后每次打开都返回同一个：反复绑定会让 skill 常驻落在
 * 最后一次的 id 上，而宿主外壳用的是当前 activeSessionId，两侧错位时
 * agent 就丢掉 skill。所以这里只绑不覆盖。
 *
 * 归属标记同样在这里落库（`sessions.workbench_id`）—— 只有打了标记，
 * 主对话列表才会把它排除掉，本工作台的列表才能查到它。标记走 HTTP
 * PATCH 而不是让桌面端直接写 sessions.db，避免和 sidecar 抢写锁。
 */
export const workbenchEnsureSession = (id: string, sessionId: string) =>
  invoke<string>("workbench_ensure_session", { id, sessionId });

/**
 * 把会话划归某个工作台（或传 null 归还给普通对话）。
 * 归属决定这个会话出现在哪个对话列表里，是两侧不互串的唯一依据。
 */
export const setSessionWorkbench = (sessionId: string, workbenchId: string | null) =>
  updateSessionMetadata(sessionId, { workbench_id: workbenchId });

/** 某个工作台自己的会话列表。不传 id 则是普通对话列表。 */
export const listWorkbenchSessions = (workbenchId?: string) =>
  getSessions(workbenchId);

/**
 * 跑 lint 规则表。返回带「文件:行号 + 规则 ID + 修法」的文本，空串表示通过。
 * 带 path/content 时做落盘前预检，不先写脏文件再回滚。
 */
export const workbenchLint = (id: string, path?: string, content?: string) =>
  invoke<string>("workbench_lint", { id, path, content });

/** 设定第三方依赖，宿主校验白名单后写回 importmap。 */
export const workbenchSetDeps = (id: string, deps: string[]) =>
  invoke<string[]>("workbench_set_deps", { id, deps });

/** 已安装的第三方库清单（import 名 → {version, entry}）。 */
export const workbenchVendorIndex = () =>
  invoke<Record<string, { version: string; entry: string }>>("workbench_vendor_index");

/** 共享 vendor 目录，依赖申请通过后库文件放这里。 */
export const workbenchVendorDir = () => invoke<string>("workbench_vendor_dir");

/** 模块索引（文件 → 导出符号），迭代前注入给 agent 复用已有模块。 */
export const workbenchModuleIndex = (id: string) =>
  invoke<string>("workbench_module_index", { id });

/** 撤销最近一次代码改动，返回被恢复的文件（无改动时 null）。 */
export const workbenchUndo = (id: string) =>
  invoke<string | null>("workbench_undo", { id });

/** 可撤销的改动列表（最近在前）。 */
export const workbenchHistory = (id: string) => invoke<string[]>("workbench_history", { id });

/**
 * 宿主网关调用。仅供宿主外壳转发 iframe 的 postMessage 请求 ——
 * 代码层拿不到直接的 IPC 入口，这是唯一的通道。
 */
export const workbenchGateway = (
  id: string,
  method: "db.query" | "db.exec" | "fs.read" | "fs.write" | "fs.list",
  params: Record<string, unknown>,
): Promise<unknown> => {
  switch (method) {
    case "db.query":
      return invoke("workbench_db_query", {
        id,
        sql: params.sql,
        params: params.params ?? [],
      });
    case "db.exec":
      return invoke("workbench_db_exec", {
        id,
        sql: params.sql,
        params: params.params ?? [],
      });
    case "fs.read":
      return invoke("workbench_fs_read", { path: params.path });
    case "fs.write":
      return invoke("workbench_fs_write", {
        path: params.path,
        content: params.content,
      });
    case "fs.list":
      return invoke("workbench_fs_list", { path: params.path });
  }
};

/**
 * 画布入口地址。走 asset 协议而不是 file:// —— file:// 下 ES module
 * 会被 CORS 拦下，importmap 与相对路径全部失效。
 *
 * 不能用 convertFileSrc：它把整个路径 encodeURIComponent，Windows 反斜杠
 * 全部变成 %5C，iframe 的 base URL 里一个路径分隔符都不剩 —— 相对资源
 * （./styles/tokens.css、../../_vendor/wb-sdk.js）会被解析到站点根，
 * asset 协议拿到 `styles/...` 这种相对路径直接 403，画布整面白屏。
 * 这里手工拼：段内编码、分隔符保留字面 `/`；Rust 侧 percent-decode 后
 * 得到 `C:/...` 正斜杠路径，File::open 与 scope 判定（先 canonicalize）
 * 都照常通过。
 */
export const workbenchEntryUrl = async (id: string): Promise<string> => {
  const path = await invoke<string>("workbench_entry_path", { id });
  const segments = path.replace(/\\/g, "/").split("/");
  return `http://asset.localhost/${segments.map(encodeURIComponent).join("/")}`;
};
