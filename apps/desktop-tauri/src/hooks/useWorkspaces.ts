// 全局工作区管理 hook。
//
// 工作区 = 一组可复用的目录别名（带 name + path），让用户能：
// 1. 在侧边栏 / ChatView chip 里快速选择"限制当前对话到哪个目录"
// 2. 后端 LocalBackend.workspace_root 用这个 path 强制路径边界
// 3. localStorage 持久化，跨会话复用
//
// 与群聊（groupchat）的区别：
// - 群聊：多成员（user+agent）对话的容器
// - 工作区：限制单次对话的读写范围（沙箱边界），是 prompt + 后端双重约束
//
// 存储 key: `hermix.desktop.workspaces`（JSON 数组 [{id, name, path, createdAt}]）

import { useCallback, useEffect, useState } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

export interface Workspace {
  id: string;
  name: string;
  path: string;
  createdAt: number;
}

const STORAGE_KEY = STORAGE_KEYS.workspaces;

function loadAll(): Workspace[] {
  try {
    const raw = storage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed.filter(
      (w): w is Workspace =>
        w &&
        typeof w.id === "string" &&
        typeof w.name === "string" &&
        typeof w.path === "string" &&
        typeof w.createdAt === "number",
    );
  } catch {
    return [];
  }
}

// 2026-07-08 (user): same-tab pub/sub for workspaces changes. The native
// `storage` event only fires cross-tab/cross-window, so two `useWorkspaces()`
// hooks living in the same tab (ChatView + Sidebar) would otherwise drift:
// ChatView's `addWorkspace` updates its own React state and persists, but
// Sidebar's React state stays stale — and any session whose
// `sessionKey(<id>, "workspace")` points at a path the Sidebar instance
// hasn't seen yet gets bucketed into "未分组" until a full reload.
// We dispatch a CustomEvent after every `saveAll` and listen for it in
// every `useWorkspaces()` instance; `StorageEvent` would be cleaner in
// intent but most browsers won't dispatch a storage event to listeners
// in the same window.
const WORKSPACES_CHANGED_EVT = "hermix:workspaces-changed";

function saveAll(list: Workspace[]): void {
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(list));
    // 2026-07-15: Defer the cross-instance sync event to a microtask
    // so it doesn't fire synchronously inside a setState callback.
    // When addWorkspace is called from a useEffect (e.g. Sidebar's
    // orphan-reconciliation or ChatView's workspace-auto-add), the
    // synchronous dispatchEvent would trigger the onChanged listener
    // in the other component's useWorkspaces instance immediately,
    // causing "Cannot update a component (`ChatView`) while rendering
    // a different component (`Sidebar`)" React warnings.
    queueMicrotask(() => {
      window.dispatchEvent(new CustomEvent(WORKSPACES_CHANGED_EVT));
    });
  } catch {
    // quota exceeded / private mode — silently ignore
  }
}

function genId(): string {
  return `ws-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

export function useWorkspaces(): {
  workspaces: Workspace[];
  add: (name: string, path: string) => Workspace;
  remove: (id: string) => void;
  rename: (id: string, name: string) => void;
  findByPath: (path: string) => Workspace | undefined;
} {
  const [workspaces, setWorkspaces] = useState<Workspace[]>(loadAll);

  // Multi-tab + same-tab sync. The native `storage` event covers
  // cross-tab writes; our custom CustomEvent covers in-tab writes
  // (a `useWorkspaces()` call site elsewhere in the same React tree).
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === STORAGE_KEY) setWorkspaces(loadAll());
    };
    const onChanged = () => setWorkspaces(loadAll());
    window.addEventListener("storage", onStorage);
    window.addEventListener(WORKSPACES_CHANGED_EVT, onChanged);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener(WORKSPACES_CHANGED_EVT, onChanged);
    };
  }, []);

  const add = useCallback((name: string, path: string): Workspace => {
    const trimmed = path.trim();
    // 2026-07-08 (user): 插入前按 path 去重，避免同目录每次都被加一条。
    const existing = workspaces.find((w) => w.path === trimmed);
    if (existing) return existing;
    const ws: Workspace = {
      id: genId(),
      name: name.trim() || trimmed.split(/[\\/]/).filter(Boolean).pop() || "未命名工作区",
      path: trimmed,
      createdAt: Date.now(),
    };
    setWorkspaces((prev) => {
      if (prev.find((w) => w.path === trimmed)) return prev; // 防 rapid 重复
      const next = [...prev, ws];
      saveAll(next);
      return next;
    });
    return ws;
  }, [workspaces]);

  const remove = useCallback((id: string) => {
    setWorkspaces((prev) => {
      const next = prev.filter((w) => w.id !== id);
      saveAll(next);
      return next;
    });
  }, []);

  const rename = useCallback((id: string, name: string) => {
    setWorkspaces((prev) => {
      const next = prev.map((w) => (w.id === id ? { ...w, name: name.trim() || w.name } : w));
      saveAll(next);
      return next;
    });
  }, []);

  const findByPath = useCallback(
    (path: string) => workspaces.find((w) => w.path === path),
    [workspaces],
  );

  return { workspaces, add, remove, rename, findByPath };
}
