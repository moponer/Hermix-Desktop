import { useCallback, useEffect, useState } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

const STORAGE_KEY = STORAGE_KEYS.workspaceRoot;

/**
 * Tracks the user's current workspace root across sessions. This is the base
 * directory the Tauri `read_artifact_file` command anchors relative paths
 * against — fixing the "LLM said `tmp/xx/方案.md` but I can't open it" bug.
 *
 * Defaults to `null`; the Tauri command will then fall back to:
 *   1. `HERMIX_DESKTOP_WORKSPACE_ROOT` env var
 *   2. The first entry of `HERMIX_DESKTOP_ARTIFACT_ROOTS`
 *   3. The user's home directory
 * So even with no value set, opening a relative path *usually* still works
 * if the user runs Hermix from their project directory.
 */
export function useWorkspaceRoot(): {
  workspaceRoot: string | null;
  setWorkspaceRoot: (next: string | null) => void;
} {
  const [workspaceRoot, setWorkspaceRootState] = useState<string | null>(() => {
    try {
      return storage.getItem(STORAGE_KEY);
    } catch {
      return null;
    }
  });

  // Re-sync if other tabs/windows change the value.
  useEffect(() => {
    const onStorage = (e: StorageEvent) => {
      if (e.key === STORAGE_KEY) {
        setWorkspaceRootState(e.newValue);
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, []);

  const setWorkspaceRoot = useCallback((next: string | null) => {
    setWorkspaceRootState(next);
    try {
      if (next) {
        storage.setItem(STORAGE_KEY, next);
      } else {
        storage.removeItem(STORAGE_KEY);
      }
    } catch {
      // localStorage may be disabled — silently no-op
    }
  }, []);

  return { workspaceRoot, setWorkspaceRoot };
}
