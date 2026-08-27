import { useEffect, useState, useCallback } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { THEMES, isKnownTheme, isDarkTheme } from "../styles/themes/registry";

// 主题 = 任一已发现的 .css (light / dark / rainforest / 用户新增)
// 数据源：themes/registry.ts 的 THEMES 数组 (auto-discover)
// 持久化键：hermix.desktop.theme
export type ThemeMode = string;

/** 主题切换后派发；非 React 集成点（ECharts 等三方实例）监听它重建。 */
export const THEME_CHANGED_EVENT = "hermix:theme-changed";

function applyTheme(id: string) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const body = document.body;
  // 单一 data-theme = 主题唯一来源
  root.dataset.theme = id;
  if (body) body.dataset.theme = id;
  // .dark 类 = 历史兼容残留（个别 css 选择器仍引用），值来自 registry
  // 元数据 —— 不存在任何硬编码主题名单。
  const wantDark = isDarkTheme(id);
  root.classList.toggle("dark", wantDark);
  if (body) body.classList.toggle("dark", wantDark);
  root.style.colorScheme = wantDark ? "dark" : "light";
  window.dispatchEvent(new CustomEvent(THEME_CHANGED_EVENT, { detail: id }));
}

function readStored(): ThemeMode {
  try {
    const v = storage.getItem(STORAGE_KEYS.theme);
    if (v && isKnownTheme(v)) return v;
  } catch {}
  return "light";
}

export function useTheme() {
  const [mode, setModeState] = useState<ThemeMode>(() => readStored());

  useEffect(() => {
    applyTheme(mode);
    try { storage.setItem(STORAGE_KEYS.theme, mode); } catch {}
  }, [mode]);

  const setMode = useCallback((next: ThemeMode) => setModeState(next), []);
  const cycle = useCallback(() => {
    setModeState((cur) => {
      const idx = THEMES.findIndex((t) => t.id === cur);
      return THEMES[(idx + 1) % THEMES.length]?.id ?? "light";
    });
  }, []);
  return { mode, setMode, cycle };
}
