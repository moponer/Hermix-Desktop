import { useEffect, useState, useCallback } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

export type ThemeMode = "light" | "dark" | "system";
const STORAGE_KEY = STORAGE_KEYS.theme;

function getSystemPrefersDark(): boolean {
  return typeof window !== "undefined" && window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches;
}

function applyTheme(mode: ThemeMode) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const wantDark = mode === "dark" || (mode === "system" && getSystemPrefersDark());
  root.classList.toggle("dark", wantDark);
}

function readStored(): ThemeMode {
  try {
    const v = storage.getItem(STORAGE_KEY);
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {}
  return "system";
}

export function useTheme() {
  const [mode, setModeState] = useState<ThemeMode>(() => readStored());

  useEffect(() => {
    applyTheme(mode);
    try { storage.setItem(STORAGE_KEY, mode); } catch {}
  }, [mode]);

  // Track system preference changes when in system mode
  useEffect(() => {
    if (mode !== "system") return;
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => applyTheme("system");
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
  }, [mode]);

  const setMode = useCallback((next: ThemeMode) => setModeState(next), []);
  const cycle = useCallback(() => {
    setModeState((cur) => (cur === "light" ? "dark" : cur === "dark" ? "system" : "light"));
  }, []);

  const resolvedDark = mode === "dark" || (mode === "system" && getSystemPrefersDark());

  return { mode, setMode, cycle, resolvedDark };
}
