// 工作台存储层的前端封装。
// 工作台在宿主内运行、无需打包，因此不走 sidecar HTTP，直接走 Tauri IPC。
import { convertFileSrc, invoke } from "@tauri-apps/api/core";

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

export const workbenchRead = (id: string, path: string) =>
  invoke<string>("workbench_read", { id, path });

export const workbenchWrite = (id: string, path: string, content: string) =>
  invoke<void>("workbench_write", { id, path, content });

export const workbenchFiles = (id: string) => invoke<string[]>("workbench_files", { id });

export const workbenchBindSession = (id: string, sessionId: string) =>
  invoke<void>("workbench_bind_session", { id, sessionId });

/**
 * 确保会话已绑定。`sessionId` 由调用方（前端）准备 —— 会话创建走 HTTP，
 * Rust 侧拿不到。
 *
 * 绑一次就够，之后每次打开都返回同一个：反复绑定会让 skill 常驻落在
 * 最后一次的 id 上，而宿主外壳用的是当前 activeSessionId，两侧错位时
 * agent 就丢掉 skill。所以这里只绑不覆盖。
 */
export const workbenchEnsureSession = (id: string, sessionId: string) =>
  invoke<string>("workbench_ensure_session", { id, sessionId });

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
 * 画布入口地址。转成 asset:// 而不是直接用 file:// ——
 * file:// 下 ES module 会被 CORS 拦下，importmap 与相对路径全部失效。
 */
export const workbenchEntryUrl = async (id: string): Promise<string> => {
  const path = await invoke<string>("workbench_entry_path", { id });
  return convertFileSrc(path, "asset");
};
