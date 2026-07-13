/**
 * storageCompat unit tests — hermix/hermes dual-namespace migration.
 *
 * Covers the 4 recommended core tests from P2-1 of FRONTEND_AUDIT_2026-07-13.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { storage, STORAGE_KEYS, sessionKey } from "./storageCompat";

function ls(): Record<string, string> {
  const out: Record<string, string> = {};
  for (let i = 0; i < window.localStorage.length; i++) {
    const k = window.localStorage.key(i);
    if (k) out[k] = window.localStorage.getItem(k) ?? "";
  }
  return out;
}

beforeEach(() => {
  window.localStorage.clear();
  window.localStorage.setItem("hermix.compat.migrated", "1"); // suppress real migration
});

describe("storageCompat", () => {
  // ── hermix/hermes dual namespace ──

  it("reads from hermix.* when present", () => {
    window.localStorage.setItem("hermix.desktop.theme", "dark");
    expect(storage.getItem(STORAGE_KEYS.theme)).toBe("dark");
  });

  it("falls back to hermes.* when hermix.* is absent", () => {
    window.localStorage.setItem("hermes.desktop.theme", "light");
    expect(storage.getItem(STORAGE_KEYS.theme)).toBe("light");
  });

  it("prefers hermix.* over hermes.* when both exist", () => {
    window.localStorage.setItem("hermes.desktop.theme", "light");
    window.localStorage.setItem("hermix.desktop.theme", "dark");
    expect(storage.getItem(STORAGE_KEYS.theme)).toBe("dark");
  });

  it("returns null when neither namespace has the key", () => {
    expect(storage.getItem(STORAGE_KEYS.theme)).toBeNull();
  });

  // ── Write path ──

  it("writes to hermix.* namespace only", () => {
    storage.setItem(STORAGE_KEYS.theme, "dark");
    expect(window.localStorage.getItem("hermix.desktop.theme")).toBe("dark");
    expect(window.localStorage.getItem("hermes.desktop.theme")).toBeNull();
  });

  it("removeItem clears hermix.* entry", () => {
    storage.setItem(STORAGE_KEYS.theme, "dark");
    storage.removeItem(STORAGE_KEYS.theme);
    expect(window.localStorage.getItem("hermix.desktop.theme")).toBeNull();
  });

  // ── Legacy migration ──

  it("migrateLegacyKeys copies hermes.* value to hermix.* when hermix.* is absent", () => {
    window.localStorage.setItem("hermes.desktop.theme", "light");
    window.localStorage.removeItem("hermix.compat.migrated"); // allow migration
    storage.migrateLegacyKeys([STORAGE_KEYS.theme]);
    expect(window.localStorage.getItem("hermix.desktop.theme")).toBe("light");
  });

  it("migrateLegacyKeys does NOT overwrite existing hermix.* value", () => {
    window.localStorage.setItem("hermes.desktop.theme", "light");
    window.localStorage.setItem("hermix.desktop.theme", "dark");
    window.localStorage.removeItem("hermix.compat.migrated");
    storage.migrateLegacyKeys([STORAGE_KEYS.theme]);
    expect(window.localStorage.getItem("hermix.desktop.theme")).toBe("dark");
  });

  it("migrateLegacyKeys is idempotent (gated by hermix.compat.migrated flag)", () => {
    window.localStorage.setItem("hermes.desktop.theme", "light");
    window.localStorage.removeItem("hermix.compat.migrated"); // allow first migration
    // First migration
    storage.migrateLegacyKeys([STORAGE_KEYS.theme]);
    expect(window.localStorage.getItem("hermix.desktop.theme")).toBe("light");

    // Change legacy value — should NOT be re-copied
    window.localStorage.setItem("hermes.desktop.theme", "changed");
    storage.migrateLegacyKeys([STORAGE_KEYS.theme]);
    expect(window.localStorage.getItem("hermix.desktop.theme")).toBe("light");
  });

  // ── sessionKey ──
  it("sessionKey generates correct hermix.session.* key", () => {
    const key = sessionKey("abc-123", "expertId");
    expect(key).toBe("hermix.session.abc-123.expertId");
  });
});
