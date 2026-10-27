// 主题：mode 三态（跟随系统 / 亮 / 暗），持久化 localStorage。
// data-theme 是唯一来源（与桌面 app 同一机制）：mode 塌缩成 light/dark
// 写进根属性，CSS 侧只认 `:root[data-theme="…"]`，不直接用媒体查询——
// 否则用户强制亮色时 OS 暗色仍会抢走 token。
import { useCallback, useEffect, useState } from "react";

export type ThemeMode = "system" | "notion" | "notion-dark";
export type ResolvedTheme = Exclude<ThemeMode, "system">;

/** 工具栏循环顺序（Notion 亮 → Notion 夜 → 跟随系统）。
    2026-10-27：旧色系（light / dark）已删除。 */
export const THEME_CYCLE: ThemeMode[] = ["notion", "notion-dark", "system"];

/** 这套主题是不是暗底 —— 语法高亮 / 共享预览的 isDark 只问这里。
    不要写 `theme === "dark"`：Notion 暗色同样需要暗底配色。 */
export function isDarkResolved(theme: ResolvedTheme): boolean {
  return theme === "notion-dark";
}

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
  return systemPrefersDark() ? "notion-dark" : "notion";
}

function apply(mode: ThemeMode): ResolvedTheme {
  const resolved = resolveTheme(mode);
  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.style.colorScheme = isDarkResolved(resolved) ? "dark" : "light";
  return resolved;
}

function readStored(): ThemeMode {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (v && (THEME_CYCLE as string[]).includes(v)) return v as ThemeMode;
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
  /** 直接指定主题（右键菜单里「浅色 / 深色 / 跟随系统」三项要用）。 */
  set: (mode: ThemeMode) => void;
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
    setMode((m) => {
      const i = THEME_CYCLE.indexOf(m);
      return THEME_CYCLE[(i + 1) % THEME_CYCLE.length];
    });
  }, []);

  return { resolved, mode, cycle, set: setMode };
}
