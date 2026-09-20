// Lightweight pin/archive persistence (frontend-only; no backend schema change).
//
// Two independent scopes — "session" / "workspace" — each on its
// own localStorage key, so deleting one object class can't pollute another
// class's ID set.
//
// Cross-component sync: a custom event keeps every in-page subscriber in
// step; separate tabs sync via the native `storage` event.
//
// The hook stays pure — it never reorders on pin (callers sort via
// sortByPinArchive), so it has zero side effects on the underlying list.
//
// Serialized as JSON.stringify([...set]); reads tolerate any corrupt data.

import { useCallback, useEffect, useState } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

export type PinnedArchivedScope = "session" | "workspace";

interface PinnedArchivedState {
  pinned: Record<PinnedArchivedScope, Set<string>>;
  archived: Record<PinnedArchivedScope, Set<string>>;
}

const EMPTY_STATE: PinnedArchivedState = {
  pinned:    { session: new Set(), workspace: new Set() },
  archived:  { session: new Set(), workspace: new Set() },
};

const STORAGE_KEYS_BY_SCOPE: Record<PinnedArchivedScope, { pinned: string; archived: string }> = {
  session:   { pinned: STORAGE_KEYS.pinnedSessions,   archived: STORAGE_KEYS.archivedSessions },
  workspace: { pinned: STORAGE_KEYS.pinnedWorkspaces, archived: STORAGE_KEYS.archivedWorkspaces },
};

function loadSet(key: string): Set<string> {
  try {
    const raw = storage.getItem(key);
    if (!raw) return new Set();
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return new Set(parsed.filter((v): v is string => typeof v === "string"));
  } catch {
    /* 损坏数据当空集处理 */
  }
  return new Set();
}

function loadAll(): PinnedArchivedState {
  return {
    pinned: {
      session:   loadSet(STORAGE_KEYS_BY_SCOPE.session.pinned),
      workspace: loadSet(STORAGE_KEYS_BY_SCOPE.workspace.pinned),
    },
    archived: {
      session:   loadSet(STORAGE_KEYS_BY_SCOPE.session.archived),
      workspace: loadSet(STORAGE_KEYS_BY_SCOPE.workspace.archived),
    },
  };
}

const CHANGED_EVT = "hermix:pinned-archived-changed";

function saveSet(key: string, set: Set<string>): void {
  try {
    storage.setItem(key, JSON.stringify([...set]));
  } catch {
    /* quota / private mode 静默 */
  }
}

function notifyChange(): void {
  // Defer to a microtask so we never dispatch the event synchronously inside
  // a React setState updater (avoids "Cannot update a component" warnings).
  queueMicrotask(() => window.dispatchEvent(new CustomEvent(CHANGED_EVT)));
}

export function usePinnedArchived() {
  const [state, setState] = useState<PinnedArchivedState>(loadAll);

  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      // 任意一个 key 变化都全量重读——简单可靠；置顶集合很小。
      const keys = Object.values(STORAGE_KEYS_BY_SCOPE).flatMap((v) => [v.pinned, v.archived]);
      if (e.key && keys.includes(e.key)) setState(loadAll());
    };
    const onChanged = () => setState(loadAll());
    window.addEventListener("storage", onStorage);
    window.addEventListener(CHANGED_EVT, onChanged);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener(CHANGED_EVT, onChanged);
    };
  }, []);

  // ── 查询 ─────────────────────────────────────────────────────────
  const isPinned = useCallback(
    (id: string, scope: PinnedArchivedScope): boolean => state.pinned[scope].has(id),
    [state],
  );
  const isArchived = useCallback(
    (id: string, scope: PinnedArchivedScope): boolean => state.archived[scope].has(id),
    [state],
  );

  // ── 变更 ─────────────────────────────────────────────────────────
  const togglePin = useCallback((id: string, scope: PinnedArchivedScope): void => {
    setState((prev) => {
      const next = new Set(prev.pinned[scope]);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      const merged: PinnedArchivedState = {
        ...prev,
        pinned: { ...prev.pinned, [scope]: next },
      };
      saveSet(STORAGE_KEYS_BY_SCOPE[scope].pinned, next);
      notifyChange();
      return merged;
    });
  }, []);

  const toggleArchive = useCallback((id: string, scope: PinnedArchivedScope): void => {
    setState((prev) => {
      const next = new Set(prev.archived[scope]);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      const merged: PinnedArchivedState = {
        ...prev,
        archived: { ...prev.archived, [scope]: next },
      };
      saveSet(STORAGE_KEYS_BY_SCOPE[scope].archived, next);
      notifyChange();
      return merged;
    });
  }, []);

  // ── 批量清理 ───────────────────────────────────────────────────
  // 真实删除某个对象后，从置顶/归档集合里清掉它，避免幽灵 ID 残留。
  const forget = useCallback((id: string, scope: PinnedArchivedScope): void => {
    setState((prev) => {
      const pinned = prev.pinned[scope];
      const archived = prev.archived[scope];
      if (!pinned.has(id) && !archived.has(id)) return prev;
      const nextPinned = new Set(pinned);
      nextPinned.delete(id);
      const nextArchived = new Set(archived);
      nextArchived.delete(id);
      const merged: PinnedArchivedState = {
        ...prev,
        pinned: { ...prev.pinned, [scope]: nextPinned },
        archived: { ...prev.archived, [scope]: nextArchived },
      };
      saveSet(STORAGE_KEYS_BY_SCOPE[scope].pinned, nextPinned);
      saveSet(STORAGE_KEYS_BY_SCOPE[scope].archived, nextArchived);
      notifyChange();
      return merged;
    });
  }, []);

  return useCallback(() => ({ isPinned, isArchived, togglePin, toggleArchive, forget }), [
    isPinned, isArchived, togglePin, toggleArchive, forget,
  ])();
}

// Re-export for tests / advanced use
export const __TEST__ = { EMPTY_STATE, loadAll, saveSet };
