// Expert + workspace + permission state hooks.
// State is persisted in localStorage so it survives reloads and session switches.
// - `hermix.session.<id>.expertId` — the expert currently hired for session <id>
// - `hermix.desktop.permission_level` — global ("default" | "full")
// - `hermix.desktop.workspace` — global default workspace (separate from per-message
// project binding; this is the *default* sent to backend as `workspace` field)
// - `hermix.desktop.recent_experts` — LRU list of recently hired expert ids
// (capped at 5) used by the ChatView expert chip so it shows only what
// the user has actually touched, not the full 18-item catalog.

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  BUILTIN_EXPERTS,
  EXPERT_CATEGORIES,
  getBuiltinExpert,
  type ExpertDef,
  type ExpertCategory,
} from "../data/builtinExperts";
import { fetchExperts, type RemoteExpert } from "../api";
import { storage, STORAGE_KEYS, sessionKey } from "../utils/storageCompat";

const PERMISSION_LEVEL_KEY = STORAGE_KEYS.permissionLevel;
const RECENT_EXPERTS_KEY = STORAGE_KEYS.recentExperts;
const RECENT_EXPERTS_MAX = 5;
export const SESSION_EXPERT_KEY = (sessionId: string) => sessionKey(sessionId, "expertId");
export const SESSION_WORKSPACE_KEY = (sessionId: string) => sessionKey(sessionId, "workspace");
export const SESSION_SKILLS_KEY = (sessionId: string) => sessionKey(sessionId, "skills");

// ── Draft storage keys ──────────────────────────────────────────────
// When the user is in the welcome state (no active session), they can
// still pick an expert / workspace from the composer chips. Without
// these draft keys, the per-session hooks had no place to write the
// choice, so `setExpert` / `setWorkspace` silently dropped the call
// and the chip stayed on "专家" / "目录" after the user picked an item.
// The draft is the source of truth while `sessionId === null` and gets
// migrated into the new session's per-session storage on first send
// (see App.tsx :: handleSendMessage).
const DRAFT_EXPERT_KEY = STORAGE_KEYS.draftExpertId;
const DRAFT_WORKSPACE_KEY = STORAGE_KEYS.draftWorkspace;

export type PermissionLevel = "default" | "full";

export interface PermissionMeta {
  id: PermissionLevel;
  label: string;
  description: string;
  examples: string[];
}

export const PERMISSION_LEVELS: PermissionMeta[] = [
  {
    id: "default",
    label: "默认权限",
    description: "高危操作需弹窗确认",
    examples: ["写文件 / 改配置", "执行命令", "网络请求", "删除操作"],
  },
  {
    id: "full",
    label: "完全权限",
    description: "所有操作允许，无需确认",
    examples: ["所有操作直接执行", "适合批量自动化任务", "出错风险由用户承担"],
  },
];

// ── usePermissionLevel ──────────────────────────────────────────────

export function usePermissionLevel(): [PermissionLevel, (lvl: PermissionLevel) => void] {
  const [level, setLevel] = useState<PermissionLevel>(() => {
    try {
      const raw = storage.getItem(PERMISSION_LEVEL_KEY);
      if (raw === "full") return "full";
    } catch {}
    return "default";
  });
  useEffect(() => {
    try {
      storage.setItem(PERMISSION_LEVEL_KEY, level);
    } catch {}
  }, [level]);
  return [level, setLevel];
}

// ── useSessionExpert ────────────────────────────────────────────────
// Bind a hired expert to a specific chat session. Returns the active
// `ExpertDef` (built-in lookup) plus a setter.

export function useSessionExpert(sessionId: string | null | undefined): {
  expert: ExpertDef | null;
  setExpert: (id: string | null) => void;
  clear: () => void;
} {
  // Draft key is the single source of truth while no session is active;
  // the per-session key is kept only for backward compatibility with older
  // stored state.
  const [expertId, setExpertId] = useState<string | null>(() => {
    try {
      const draft = storage.getItem(DRAFT_EXPERT_KEY);
      if (draft) return draft;
    } catch {}
    // Fallback: legacy per-session key.
    if (sessionId) {
      try {
        return storage.getItem(SESSION_EXPERT_KEY(sessionId));
      } catch {}
    }
    return null;
  });

  useEffect(() => {
    // No re-read needed when sessionId changes: the draft key is untouched.
    if (sessionId) {
      try {
        const draft = storage.getItem(DRAFT_EXPERT_KEY);
        if (!draft) {
          const session = storage.getItem(SESSION_EXPERT_KEY(sessionId));
          if (session) {
            storage.setItem(DRAFT_EXPERT_KEY, session);
            setExpertId(session);
          }
        }
      } catch {}
    }
  }, [sessionId]);

  const setExpert = useCallback(
    (id: string | null) => {
      // Write both slots so older readers that only know the session key still work.
      try {
        if (id) {
          storage.setItem(DRAFT_EXPERT_KEY, id);
          if (sessionId) storage.setItem(SESSION_EXPERT_KEY(sessionId), id);
          const raw = storage.getItem(RECENT_EXPERTS_KEY);
          let ids: string[] = [];
          try { const p = JSON.parse(raw || '[]'); if (Array.isArray(p)) ids = p; } catch {}
          const without = ids.filter((x) => x !== id);
          ids = [id, ...without].slice(0, RECENT_EXPERTS_MAX);
          storage.setItem(RECENT_EXPERTS_KEY, JSON.stringify(ids));
        } else {
          storage.removeLegacy(DRAFT_EXPERT_KEY);
          if (sessionId) storage.removeLegacy(SESSION_EXPERT_KEY(sessionId));
        }
      } catch {}
      setExpertId(id);
    },
    [sessionId],
  );

  const clear = useCallback(() => setExpert(null), [setExpert]);

  const expert = expertId ? getBuiltinExpert(expertId) ?? null : null;
  return { expert, setExpert, clear };
}

// ── useExpertCatalog ────────────────────────────────────────────────
// Fetch the live expert catalog from the backend. Falls back to the
// locally-bundled BUILTIN_EXPERTS for the first paint so the UI is
// never empty, and re-fetches whenever the active session changes so
// hired state stays in sync.

function remoteToExpertDef(r: RemoteExpert): ExpertDef {
  return {
    id: r.id,
    name: r.name,
    avatar: r.avatar,
    role: r.role,
    category: r.category as ExpertCategory,
    description: r.description,
    systemPrompt: r.system_prompt,
    toolset: r.toolset ?? undefined,
    isBuiltin: r.is_builtin,
  };
}

export function useExpertCatalog(): {
  experts: ExpertDef[];
  categories: ExpertCategory[];
  byCategory: Record<string, ExpertDef[]>;
  loading: boolean;
  source: "backend" | "builtin-fallback";
} {
  const [remote, setRemote] = useState<RemoteExpert[] | null>(null);
  const [loading, setLoading] = useState<boolean>(true);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    fetchExperts()
      .then((list) => {
        if (cancelled) return;
        if (list && list.length > 0) {
          setRemote(list);
        } else {
          setRemote(null);
        }
      })
      .catch(() => {
        if (cancelled) return;
        setRemote(null);
      })
      .finally(() => {
        if (cancelled) return;
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const source: "backend" | "builtin-fallback" = remote ? "backend" : "builtin-fallback";
  const experts: ExpertDef[] = useMemo(
    () => (remote && remote.length > 0 ? remote.map(remoteToExpertDef) : BUILTIN_EXPERTS),
    [remote],
  );
  const byCategory: Record<string, ExpertDef[]> = useMemo(
    () =>
      experts.reduce(
        (acc, e) => {
          (acc[e.category] ||= []).push(e);
          return acc;
        },
        {} as Record<string, ExpertDef[]>,
      ),
    [experts],
  );
  // Preserve the curated category order; append any unknown categories
  // surfaced by the backend at the end.
  const categories: ExpertCategory[] = useMemo(() => {
    const known = new Set<string>(EXPERT_CATEGORIES as string[]);
    const out: ExpertCategory[] = [...EXPERT_CATEGORIES];
    for (const cat of Object.keys(byCategory)) {
      if (!known.has(cat)) out.push(cat as ExpertCategory);
    }
    return out;
  }, [byCategory]);

  return { experts, categories, byCategory, loading, source };
}

// ── useRecentExperts ────────────────────────────────────────────────
// Persist a small LRU (cap 5) of expert ids the user has hired recently.
// Used by the ChatView expert chip to decide which entries to surface in
// the popover — the rest live in the Expert Center and are reached via
// the "查看全部 →" link.

export function useRecentExperts(): {
  recent: ExpertDef[];
  markUsed: (id: string) => void;
  clear: () => void;
} {
  const [ids, setIds] = useState<string[]>(() => {
    try {
      const raw = storage.getItem(RECENT_EXPERTS_KEY);
      if (!raw) return [];
      const parsed = JSON.parse(raw) as unknown;
      if (!Array.isArray(parsed)) return [];
      return parsed.filter((x): x is string => typeof x === "string").slice(0, RECENT_EXPERTS_MAX);
    } catch {
      return [];
    }
  });

  useEffect(() => {
    try {
      storage.setItem(RECENT_EXPERTS_KEY, JSON.stringify(ids));
    } catch {}
  }, [ids]);

  const markUsed = useCallback((id: string) => {
    setIds((prev) => {
      const without = prev.filter((x) => x !== id);
      return [id, ...without].slice(0, RECENT_EXPERTS_MAX);
    });
  }, []);

  const clear = useCallback(() => {
    setIds([]);
  }, []);

  const recent: ExpertDef[] = useMemo(
    () => ids.map((id) => getBuiltinExpert(id)).filter((e): e is ExpertDef => !!e),
    [ids],
  );

  return { recent, markUsed, clear };
}

// ── useSessionWorkspace ─────────────────────────────────────────────
// Persist the currently-bound workspace path per session. This becomes
// the `workspace` field in the WS send payload — used by the backend to
// enforce path boundaries on file/terminal tools.

export function useSessionWorkspace(sessionId: string | null | undefined): {
  workspace: string | null;
  setWorkspace: (path: string | null) => void;
} {
  const [workspace, setWorkspaceState] = useState<string | null>(() => {
    if (sessionId) {
      try {
        return storage.getItem(SESSION_WORKSPACE_KEY(sessionId));
      } catch {
        return null;
      }
    }
    // No active session: fall back to the draft slot so the workspace
    // chip shows whatever the user last picked in the welcome UI.
    try {
      return storage.getItem(DRAFT_WORKSPACE_KEY);
    } catch {
      return null;
    }
  });

  useEffect(() => {
    if (sessionId) {
      try {
        const stored = storage.getItem(SESSION_WORKSPACE_KEY(sessionId));
        setWorkspaceState(stored);
      } catch {
        setWorkspaceState(null);
      }
    } else {
      // Switched to welcome state (or stayed there): sync from draft.
      try {
        setWorkspaceState(storage.getItem(DRAFT_WORKSPACE_KEY));
      } catch {
        setWorkspaceState(null);
      }
    }
  }, [sessionId]);

  const setWorkspace = useCallback(
    (path: string | null) => {
      if (sessionId) {
        try {
          if (path) {
            storage.setItem(SESSION_WORKSPACE_KEY(sessionId), path);
          } else {
            storage.removeLegacy(SESSION_WORKSPACE_KEY(sessionId));
          }
        } catch {}
      } else {
        // No session yet: write to the draft slot so the chip text
        // updates immediately and the selection carries forward when
        // the user finally sends their first message (see
        // App.tsx :: handleSendMessage).
        try {
          if (path) {
            storage.setItem(DRAFT_WORKSPACE_KEY, path);
          } else {
            storage.removeLegacy(DRAFT_WORKSPACE_KEY);
          }
        } catch {}
      }
      setWorkspaceState(path);
    },
    [sessionId],
  );

  return { workspace, setWorkspace };
}

// ── useSessionSkills ─────────────────────────────────────────────
// Ephemeral per-session skill set. The server's `skill_store` decides
// which skills are *available*, but the user can pick a subset for the
// current chat (e.g. "this conversation is about git → enable git skill").
// The selected names ride along on every send as `overrides.skills`; the
// backend injects them into `skills_system_prompt` on top of the
// server-enabled set (so server-enable is the floor, not the ceiling).

export function useSessionSkills(sessionId: string | null | undefined): {
  skills: string[];
  setSkills: (names: string[]) => void;
  addSkill: (name: string) => void;
  removeSkill: (name: string) => void;
  clear: () => void;
} {
  const [skills, setSkillsState] = useState<string[]>(() => {
    if (!sessionId) return [];
    try {
      const raw = storage.getItem(SESSION_SKILLS_KEY(sessionId));
      if (!raw) return [];
      const parsed = JSON.parse(raw) as unknown;
      if (!Array.isArray(parsed)) return [];
      return parsed.filter((x): x is string => typeof x === "string");
    } catch {
      return [];
    }
  });

  useEffect(() => {
    if (!sessionId) {
      setSkillsState([]);
      return;
    }
    try {
      const raw = storage.getItem(SESSION_SKILLS_KEY(sessionId));
      if (!raw) {
        setSkillsState([]);
        return;
      }
      const parsed = JSON.parse(raw) as unknown;
      if (Array.isArray(parsed)) {
        setSkillsState(parsed.filter((x): x is string => typeof x === "string"));
      } else {
        setSkillsState([]);
      }
    } catch {
      setSkillsState([]);
    }
  }, [sessionId]);

  const persist = useCallback(
    (next: string[]) => {
      setSkillsState(next);
      if (!sessionId) return;
      try {
        if (next.length === 0) {
          storage.removeLegacy(SESSION_SKILLS_KEY(sessionId));
        } else {
          storage.setItem(SESSION_SKILLS_KEY(sessionId), JSON.stringify(next));
        }
      } catch {}
    },
    [sessionId],
  );

  const setSkills = useCallback((names: string[]) => {
    // de-dup + drop empties
    const cleaned = Array.from(new Set(names.map((n) => n.trim()).filter(Boolean)));
    persist(cleaned);
  }, [persist]);

  const addSkill = useCallback((name: string) => {
    const trimmed = name.trim();
    if (!trimmed) return;
    setSkillsState((prev) => {
      if (prev.includes(trimmed)) return prev;
      const next = [...prev, trimmed];
      if (sessionId) {
        try {
          storage.setItem(SESSION_SKILLS_KEY(sessionId), JSON.stringify(next));
        } catch {}
      }
      return next;
    });
  }, [sessionId]);

  const removeSkill = useCallback((name: string) => {
    setSkillsState((prev) => {
      const next = prev.filter((n) => n !== name);
      if (sessionId) {
        try {
          if (next.length === 0) {
            storage.removeLegacy(SESSION_SKILLS_KEY(sessionId));
          } else {
            storage.setItem(SESSION_SKILLS_KEY(sessionId), JSON.stringify(next));
          }
        } catch {}
      }
      return next;
    });
  }, [sessionId]);

  const clear = useCallback(() => persist([]), [persist]);

  return { skills, setSkills, addSkill, removeSkill, clear };
}
