// localStorage compatibility shim for the hermes → hermix rename.
// On read: try the new `hermix.*` key first, then fall back to the
// legacy `hermes.*` key so users on a pre-rename install don't lose
// their settings. On write: write only to the new key — the legacy
// key is left in place until the explicit `removeLegacy` form.
//
// `STORAGE_KEYS` is the only canonical source for these strings; the
// rest of the codebase must import from here (not use literals) so a
// future rebrand touches one file.

const LEGACY_PREFIX = "hermes";
const CURRENT_PREFIX = "hermix";

// Strips the leading `hermix.` / `hermes.` prefix; returns the bare
// suffix like `desktop.theme`.
function bareKey(fullKey: string): string {
  if (fullKey.startsWith(`${CURRENT_PREFIX}.`)) {
    return fullKey.slice(CURRENT_PREFIX.length + 1);
  }
  if (fullKey.startsWith(`${LEGACY_PREFIX}.`)) {
    return fullKey.slice(LEGACY_PREFIX.length + 1);
  }
  return fullKey;
}

function newKey(bare: string): string {
  return `${CURRENT_PREFIX}.${bare}`;
}

function legacyKey(bare: string): string {
  return `${LEGACY_PREFIX}.${bare}`;
}

export const storage = {
  // Reads `hermix.<key>` then falls back to `hermes.<key>`. Returns
  // null when neither exists (matches `localStorage.getItem`).
  getItem(fullKey: string): string | null {
    const bare = bareKey(fullKey);
    const v = window.localStorage.getItem(newKey(bare));
    if (v !== null) return v;
    return window.localStorage.getItem(legacyKey(bare));
  },

  // Writes only to `hermix.<key>`; the legacy `hermes.<key>` is NOT
  // deleted automatically — use `removeLegacy` for the explicit form.
  setItem(fullKey: string, value: string): void {
    const bare = bareKey(fullKey);
    window.localStorage.setItem(newKey(bare), value);
  },

  // Removes `hermix.<key>` (the user-clears-a-setting path).
  // Use `removeLegacy` to delete the old `hermes.<key>` instead.
  removeItem(fullKey: string): void {
    const bare = bareKey(fullKey);
    window.localStorage.removeItem(newKey(bare));
  },

  // Removes the legacy `hermes.<key>` entry. Safe to call when the
  // legacy key doesn't exist; used after migrate-on-boot confirms
  // the value has been copied forward.
  removeLegacy(fullKey: string): void {
    const bare = bareKey(fullKey);
    window.localStorage.removeItem(legacyKey(bare));
  },

  // One-shot migration: for each full `hermix.*` key, copy the
  // legacy `hermes.*` value forward when the new key is missing.
  // Run once on startup (gated by `hermix.compat.migrated` so it
  // runs at most once per browser profile).
  //
  // Set `hermix.compat.debug=1` (or run a dev build) to log every
  // visited key and flag conflicts where both namespaces hold data.
  // On conflict we keep the current `hermix.*` value (conservative —
  // never silently overwrite user data).
  migrateLegacyKeys(keys: readonly string[]): void {
    if (typeof window === "undefined") return;
    const debug =
      window.localStorage.getItem(`${CURRENT_PREFIX}.compat.debug`) === "1" ||
      (typeof import.meta !== "undefined" &&
        (import.meta as { env?: { DEV?: boolean } }).env?.DEV === true);
    if (window.localStorage.getItem(`${CURRENT_PREFIX}.compat.migrated`) === "1") {
      return;
    }
    for (const fullKey of keys) {
      const bare = bareKey(fullKey);
      const lk = legacyKey(bare);
      const nk = newKey(bare);
      const legacyVal = window.localStorage.getItem(lk);
      const currentVal = window.localStorage.getItem(nk);
      if (legacyVal === null) continue;
      if (currentVal !== null) {
        // Both namespaces hold data — keep the current `hermix.*`
        // value, leave the legacy key in place (never silently
        // overwrite). Surface it when debug is on.
        if (debug) {
          console.info(
            `[storageCompat] conflict on "${bare}": keeping hermix value, legacy hermes value ignored. ` +
              `set hermix.compat.debug=0 or clear hermes.${bare} to silence.`,
          );
        }
        continue;
      }
      if (debug) {
        console.info(`[storageCompat] migrating hermes.${bare} → hermix.${bare}`);
      }
      window.localStorage.setItem(nk, legacyVal);
    }
    window.localStorage.setItem(`${CURRENT_PREFIX}.compat.migrated`, "1");
  },
};

// Canonical list of localStorage keys used by desktop-tauri. Add new
// keys here; reference them as `STORAGE_KEYS.<name>` from the hook
// or component that needs them.
export const STORAGE_KEYS = {
  theme: "hermix.desktop.theme",
  sidebarWidth: "hermix.desktop.sidebar.width",
  rightWidth: "hermix.desktop.right.width",
  rightOpen: "hermix.desktop.right.open",
  activeSession: "hermix.desktop.active_session_id",
  agents: "hermix-agents",
  recentExperts: "hermix.desktop.recent_experts",
  permissionLevel: "hermix.desktop.permission_level",
  modelConfig: "hermix.desktop.model_config",
  modelCache: "hermix.desktop.model_cache",
  workspaceRoot: "hermix.desktop.workspace_root",
  workspaces: "hermix.desktop.workspaces",
  // Welcome-state draft selections — chosen before a session exists,
  // migrated to per-session storage on first send.
  draftExpertId: "hermix.desktop.draft.expertId",
  draftWorkspace: "hermix.desktop.draft.workspace",
  groupChatRooms: "hermix-chat-rooms",
  groupChatMessages: "hermix-groupchat-messages",
  mcpServers: "hermix.mcp.servers",
  // Agents view generate-flow: backend URL + key (per-user override).
  agentsServerUrl: "hermix.agents.server_url",
  agentsApiKey: "hermix.agents.api_key",
  advancedConfig: "hermix.desktop.advanced_config",
  pythonOverride: "hermix.python.override",
  // Sidebar: 置顶 / 归档（按 scope 分 key,避免一类对象的 ID 影响另一类）。
  pinnedSessions:    "hermix.desktop.pinned.sessions",
  pinnedWorkspaces:  "hermix.desktop.pinned.workspaces",
  pinnedRooms:       "hermix.desktop.pinned.rooms",
  archivedSessions:  "hermix.desktop.archived.sessions",
  archivedWorkspaces:"hermix.desktop.archived.workspaces",
  archivedRooms:     "hermix.desktop.archived.rooms",
  // 本地用户档案（无登录体系，纯本机可编辑）：{displayName, avatarDataUrl, handle}
  userProfile: "hermix.user.profile",
  // 图片生成本地偏好（前端持久化，图片生成能力接入前作为集中配置入口）
  imageConfig: "hermix.config.image",
} as const;

export type StorageKey = (typeof STORAGE_KEYS)[keyof typeof STORAGE_KEYS];

// Flat list of every canonical storage key, derived from `STORAGE_KEYS`.
// `migrateLegacyKeys` consumes this so any new key added to
// `STORAGE_KEYS` is automatically included in the hermes → hermix pass —
// no second hand-maintained list to keep in sync.
export const KNOWN_KEYS: readonly string[] = Object.values(STORAGE_KEYS);

// Builds the namespaced per-session key, e.g. `hermix.session.<id>.expertId`.
export function sessionKey(sessionId: string, suffix: string): string {
  return `${CURRENT_PREFIX}.session.${sessionId}.${suffix}`;
}
