/**
 * localStorage compatibility shim for the hermes -> hermix rename.
 *
 * On read: try the new `hermix.*` key first; if absent, fall back to the
 * legacy `hermes.*` key (so users on a pre-rename install don't lose their
 * theme / sidebar width / active session / MCP server list / etc.).
 *
 * On write: write only to the new `hermix.*` key. The legacy key is
 * deliberately left in place (we keep it conservative: don't mutate
 * user data without an explicit opt-in).
 *
 * The `STORAGE_KEYS` constants in this file are the only canonical
 * locations for these strings — the rest of the desktop-tauri codebase
 * should import from here instead of using string literals, so that
 * any future rebrand only needs to touch one file.
 *
 * ## Key naming convention
 *
 * All keys in `STORAGE_KEYS` are stored as full strings (e.g.
 * `hermix.desktop.theme`). The "bare key" portion after the prefix
 * (e.g. `desktop.theme`) is what gets passed to the helper's
 * `getItem` / `setItem` / `removeLegacy` methods.
 */

const LEGACY_PREFIX = "hermes";
const CURRENT_PREFIX = "hermix";

/**
 * Strip the leading `hermix.` (or `hermes.`) prefix from a key.
 * Returns the bare suffix like `desktop.theme`.
 */
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
  /**
   * Read a value, trying the new `hermix.<key>` entry first then the
   * legacy `hermes.<key>` entry. Returns `null` if neither exists
   * (matches `localStorage.getItem`).
   */
  getItem(fullKey: string): string | null {
    const bare = bareKey(fullKey);
    const v = window.localStorage.getItem(newKey(bare));
    if (v !== null) return v;
    return window.localStorage.getItem(legacyKey(bare));
  },

  /**
   * Write a value under the new `hermix.<key>` key. The legacy
   * `hermes.<key>` key is NOT deleted automatically — see
   * `removeLegacy` for the explicit form.
   */
  setItem(fullKey: string, value: string): void {
    const bare = bareKey(fullKey);
    window.localStorage.setItem(newKey(bare), value);
  },

  /**
   * Remove the value under the new `hermix.<key>` key. Use this when
   * the user clears a setting (vs `removeLegacy` which only deletes
   * the old key).
   */
  removeItem(fullKey: string): void {
    const bare = bareKey(fullKey);
    window.localStorage.removeItem(newKey(bare));
  },

  /**
   * Explicitly delete the legacy `hermes.<key>` entry. Use this when
   * you want to retire the legacy value (e.g. after a successful
   * migrate-on-boot, or after the user has confirmed a value is
   * migrated). Safe to call even if the legacy key doesn't exist.
   */
  removeLegacy(fullKey: string): void {
    const bare = bareKey(fullKey);
    window.localStorage.removeItem(legacyKey(bare));
  },

  /**
   * One-shot migration helper. For each key in the supplied list
   * (full `hermix.*` form, as defined in `STORAGE_KEYS`), if the
   * legacy `hermes.*` entry exists and the new `hermix.*` entry
   * does NOT, copy the legacy value forward.
   *
   * Call this once on desktop-tauri startup, after first paint
   * (so we don't block the splash) and gated behind a
   * `localStorage.getItem("hermix.compat.migrated") !== "1"` flag
   * so the work happens at most once per browser profile.
   *
   * Dual-namespace debug (P2-7): set `hermix.compat.debug=1` (or run a
   * dev build) to log every visited key and flag conflicts where both
   * `hermes.*` and `hermix.*` hold data. When both exist we keep the
   * current `hermix.*` value (conservative — never silently overwrite).
   */
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
        // Conflict: both namespaces hold data. We keep the current `hermix.*`
        // value and leave the legacy key in place (conservative: never
        // silently overwrite user data). Surface it when debug is on.
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

/**
 * Canonical list of localStorage keys used by desktop-tauri. Add new
 * keys here; reference them as `STORAGE_KEYS.<name>` from the hook
 * or component that needs them.
 */
export const STORAGE_KEYS = {
  // theme
  theme: "hermix.desktop.theme",
  // sidebar / right panel widths
  sidebarWidth: "hermix.desktop.sidebar.width",
  rightWidth: "hermix.desktop.right.width",
  rightOpen: "hermix.desktop.right.open",
  // session
  activeSession: "hermix.desktop.active_session_id",
  // agents / experts
  agents: "hermix-agents",
  recentExperts: "hermix.desktop.recent_experts",
  permissionLevel: "hermix.desktop.permission_level",
  // model config
  modelConfig: "hermix.desktop.model_config",
  modelCache: "hermix.desktop.model_cache",
  // workspaces
  workspaceRoot: "hermix.desktop.workspace_root",
  workspaces: "hermix.desktop.workspaces",
  // draft (welcome-state) selections: chosen in the welcome UI before a
  // session exists. Migrated to per-session storage on first send.
  draftExpertId: "hermix.desktop.draft.expertId",
  draftWorkspace: "hermix.desktop.draft.workspace",
  // group chat
  groupChatRooms: "hermix-chat-rooms",
  groupChatMessages: "hermix-groupchat-messages",
  // mcp
  mcpServers: "hermix.mcp.servers",
  // agents view generate-flow: backend URL + key (per-user override)
  agentsServerUrl: "hermix.agents.server_url",
  agentsApiKey: "hermix.agents.api_key",
  // advanced config (Settings → Advanced tab)
  advancedConfig: "hermix.desktop.advanced_config",
  // python runtime override
  pythonOverride: "hermix.python.override",
} as const;

export type StorageKey = (typeof STORAGE_KEYS)[keyof typeof STORAGE_KEYS];

/**
 * Flat list of every canonical storage key, derived from `STORAGE_KEYS`.
 * Used by `migrateLegacyKeys` so that any new key added to `STORAGE_KEYS`
 * is automatically included in the hermes → hermix migration pass — no
 * need to keep a second hand-maintained list in sync (P2-2).
 */
export const KNOWN_KEYS: readonly string[] = Object.values(STORAGE_KEYS);

/**
 * Build the namespaced per-session key. Sessions are user-owned data
 * kept under the new prefix (e.g. `hermix.session.<id>.expertId`).
 */
export function sessionKey(sessionId: string, suffix: string): string {
  return `${CURRENT_PREFIX}.session.${sessionId}.${suffix}`;
}
