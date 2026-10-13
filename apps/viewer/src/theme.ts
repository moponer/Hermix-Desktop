// 主题：mode 三态（跟随系统 / 亮 / 暗），持久化 localStorage。
// data-theme 是唯一来源（与桌面 app 同一机制）：mode 塌缩成 light/dark
// 写进根属性，CSS 侧只认 `:root[data-theme="…"]`，不直接用媒体查询——
// 否则用户强制亮色时 OS 暗色仍会抢走 token。
import { useCallback, useEffect, useState } from "react";

export type ThemeMode = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

const STORAGE_KEY = "hermix.viewer.theme";
const MQ_DARK = "(prefers-color-scheme: dark)";

function systemPrefersDark(): boolean {
  return (
    typeof window !== "undefined" &&
    !!window.matchMedia &&
    window.matchMedia(MQ_DARK).matches
  );
}

export function resolveTheme(mode: ThemeMode): ResolvedTheme {
  if (mode !== "system") return mode;
  return systemPrefersDark() ? "dark" : "light";
}

function apply(mode: ThemeMode): ResolvedTheme {
  const resolved = resolveTheme(mode);
  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.style.colorScheme = resolved;
  return resolved;
}

function readStored(): ThemeMode {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v === "light" || v === "dark" || v === "system") return v;
  } catch {
    /* localStorage 不可用（如禁存策略）：退回跟随系统 */
  }
  return "system";
}

/** 返回塌缩后的主题（传给共享预览的 isDark）与三态循环切换。 */
export function useTheme(): {
  resolved: ResolvedTheme;
  mode: ThemeMode;
  cycle: () => void;
} {
  const [mode, setMode] = useState<ThemeMode>(readStored);
  const [resolved, setResolved] = useState<ResolvedTheme>(() => apply(readStored()));

  useEffect(() => {
    setResolved(apply(mode));
    try {
      localStorage.setItem(STORAGE_KEY, mode);
    } catch {
      /* 忽略持久化失败，主题仅本次会话生效 */
    }
  }, [mode]);

  // system 模式下跟随 OS 实时切换
  useEffect(() => {
    if (mode !== "system" || !window.matchMedia) return;
    const mq = window.matchMedia(MQ_DARK);
    const onChange = () => setResolved(apply("system"));
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [mode]);

  const cycle = useCallback(() => {
    setMode((m) => (m === "system" ? "light" : m === "light" ? "dark" : "system"));
  }, []);

  return { resolved, mode, cycle };
}
