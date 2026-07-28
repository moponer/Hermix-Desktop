﻿﻿﻿import { useEffect, useState, useCallback } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { THEMES, isKnownTheme } from "../styles/themes/registry";

// 主题 = 任一已发现的 .css (light / dark / rainforest / 用户新增)
// 数据源：themes/registry.ts 的 THEMES 数组 (auto-discover)
// 持久化键：hermix.desktop.theme
export type ThemeMode = string;

function applyTheme(id: string) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const body = document.body;
  // 单一 data-theme = 主题唯一来源
  root.dataset.theme = id;
  if (body) body.dataset.theme = id;
  // 暗色主题集合：这些主题需要 colorScheme=dark 以正确渲染原生控件（滚动条等）
  // 同时 dark 主题挂 .dark 以兼容 tokens.css 的 .dark 选择器
  const DARK_THEMES = new Set(["dark", "rainforest"]);
  const wantDark = DARK_THEMES.has(id);
  root.classList.toggle("dark", id === "dark");
  if (body) body.classList.toggle("dark", id === "dark");
  root.style.colorScheme = wantDark ? "dark" : "light";
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

  // 主题不依赖 prefers-color-scheme (system 已退出) -- 保留监听以备扩展.
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const handler = () => { if (mode === "dark") applyTheme("dark"); };
    mq.addEventListener("change", handler);
    return () => mq.removeEventListener("change", handler);
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
