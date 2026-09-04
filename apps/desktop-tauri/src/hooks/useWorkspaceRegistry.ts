// 工作台注册表 — 模块级单例 store，跨组件共享工作台列表 / 激活项 / 详情。
//
// 存在原因：工作台页面是一块画布（只渲染 LLM 产出的内容），所有管理动作
// （新建 / 重命名 / 删除 / 模板 / 导入 / 导出）都搬到左侧栏二级菜单，两侧
// 必须看到同一份状态，靠 props 逐层透传会把 App 拖进无关的状态管理。
//
// revision：写操作计数器。画布的数据加载 effect 依赖它，侧栏撤销 / 导入后
// 递增即可让画布重新拉表数据，无需反向回调。

import { useEffect, useSyncExternalStore } from "react";
import * as api from "../api";
import type { WorkspaceDetail, WorkspaceEntry } from "../types";
import { toastError, toastSuccess } from "./useToast";

export type WorkspaceImportMode = "new" | "merge" | "upsert";

interface RegistryState {
  entries: WorkspaceEntry[];
  activeId: string | null;
  detail: WorkspaceDetail | null;
  loading: boolean;
  /** 写操作计数：撤销 / 导入 / 权限变更后递增，触发画布重拉数据。 */
  revision: number;
}

let state: RegistryState = {
  entries: [],
  activeId: null,
  detail: null,
  loading: true,
  revision: 0,
};

const listeners = new Set<() => void>();
/** 详情加载序号：切换工作台时旧请求返回后丢弃，防竞态覆盖。 */
let detailSeq = 0;
let booted = false;

function emit(): void {
  for (const l of [...listeners]) l();
}

function patch(next: Partial<RegistryState>): void {
  state = { ...state, ...next };
  emit();
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

function getSnapshot(): RegistryState {
  return state;
}

async function loadDetail(id: string | null): Promise<void> {
  const seq = ++detailSeq;
  if (!id) {
    patch({ detail: null, loading: false });
    return;
  }
  patch({ loading: true });
  try {
    const detail = await api.getWorkspace(id);
    if (seq !== detailSeq) return;
    patch({ detail, loading: false });
  } catch (e) {
    if (seq !== detailSeq) return;
    toastError(`加载工作台详情失败: ${String(e)}`);
    patch({ detail: null, loading: false });
  }
}

export async function refreshWorkspaces(): Promise<void> {
  try {
    const entries = await api.listWorkspaces();
    const activeId =
      state.activeId && entries.some((e) => e.id === state.activeId)
        ? state.activeId
        : entries[0]?.id ?? null;
    const stale = state.detail?.id !== activeId;
    patch({ entries, activeId });
    if (stale) void loadDetail(activeId);
    else patch({ loading: false });
  } catch (e) {
    toastError(`加载工作台失败: ${String(e)}`);
    patch({ loading: false });
  }
}

export function setActiveWorkspace(id: string | null): void {
  if (id === state.activeId) return;
  patch({ activeId: id });
  void loadDetail(id);
}

/** 返回是否成功：调用方据此决定是否关闭输入/面板。 */
export async function createWorkspace(name: string): Promise<boolean> {
  const trimmed = name.trim();
  if (!trimmed) return false;
  try {
    const { id } = await api.createWorkspace(trimmed);
    toastSuccess(`工作台「${trimmed}」已创建`);
    await refreshWorkspaces();
    setActiveWorkspace(id);
    return true;
  } catch (e) {
    toastError(`创建工作台失败: ${String(e)}`);
    return false;
  }
}

export async function createWorkspaceFromTemplate(tplId: string): Promise<boolean> {
  try {
    const { id, name, smoke } = await api.createWorkspaceFromTemplate(tplId);
    toastSuccess(`已按模板创建「${name}」（${smoke.length} 项自测通过）`);
    await refreshWorkspaces();
    setActiveWorkspace(id);
    return true;
  } catch (e) {
    toastError(`按模板创建失败: ${String(e)}`);
    return false;
  }
}

export async function renameWorkspace(id: string, name: string): Promise<void> {
  const trimmed = name.trim();
  if (!trimmed) return;
  try {
    await api.renameWorkspace(id, trimmed);
    toastSuccess("已重命名");
    await refreshWorkspaces();
  } catch (e) {
    toastError(`重命名失败: ${String(e)}`);
  }
}

export async function deleteWorkspace(id: string): Promise<void> {
  try {
    await api.deleteWorkspace(id);
    toastSuccess("工作台已删除");
    if (state.activeId === id) patch({ activeId: null, detail: null });
    await refreshWorkspaces();
  } catch (e) {
    toastError(`删除失败: ${String(e)}`);
  }
}

/** 权限 / schema 等写回后同步 store 详情，画布与设置面板立即可见。 */
export function applyWorkspaceDetail(detail: WorkspaceDetail): void {
  patch({ detail });
}

export function bumpWorkspaceData(): void {
  patch({ revision: state.revision + 1 });
}

export async function exportWorkspaceFile(id: string): Promise<void> {
  try {
    const blob = await api.exportWorkspace(id);
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = `${id}.hwx`;
    document.body.appendChild(a);
    a.click();
    a.remove();
    URL.revokeObjectURL(url);
    toastSuccess("工作台已导出 .hwx");
  } catch (e) {
    toastError(`导出失败: ${String(e)}`);
  }
}

export async function importWorkspacePackage(
  pkg: Record<string, unknown>,
  mode: WorkspaceImportMode,
): Promise<void> {
  try {
    const res = await api.importWorkspace(pkg, mode);
    const rows = Object.values(res.tables ?? {}).reduce<number>((s, n) => s + n, 0);
    const verb = mode === "new" ? "新建" : mode === "merge" ? "合并" : "增量同步";
    toastSuccess(`已${verb}「${String(pkg.name ?? res.id)}」（${rows} 行数据）`);
    await refreshWorkspaces();
    setActiveWorkspace(res.id);
  } catch (e) {
    toastError(`导入失败: ${String(e)}`);
  }
}

/** 订阅注册表。首个订阅者触发一次列表加载。 */
export function useWorkspaceRegistry(): RegistryState {
  useEffect(() => {
    if (booted) return;
    booted = true;
    void refreshWorkspaces();
  }, []);
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
