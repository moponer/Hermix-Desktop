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

function saveAll(list: Workspace[]): void {
  try {
    storage.setItem(STORAGE_KEY, JSON.stringify(list));
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

  // Multi-tab sync
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === STORAGE_KEY) setWorkspaces(loadAll());
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  const add = useCallback((name: string, path: string): Workspace => {
    const ws: Workspace = {
      id: genId(),
      name: name.trim() || path.split(/[\\/]/).filter(Boolean).pop() || "未命名工作区",
      path: path.trim(),
      createdAt: Date.now(),
    };
    setWorkspaces((prev) => {
      const next = [...prev, ws];
      saveAll(next);
      return next;
    });
    return ws;
  }, []);

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
