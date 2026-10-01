import { useEffect, useState, useCallback } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { THEMES, isKnownTheme, isDarkTheme } from "../styles/themes/registry";

// 主题 = 任一已发现的 .css（内置仅 light / dark 两套；用户仍可自建 .css 放入 themes/）
// 数据源：themes/registry.ts 的 THEMES 数组 (auto-discover)
// 持久化键：hermix.desktop.theme
export type ThemeMode = string;

/** 主题切换后派发；非 React 集成点（ECharts 等三方实例）监听它重建。 */
export const THEME_CHANGED_EVENT = "hermix:theme-changed";

function applyTheme(id: string) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  const body = document.body;
  // 单一 data-theme = 主题唯一来源（语义 token 由 themes/<id>.css 按
  // :root[data-theme="<id>"] 提供整套取值；.dark 类已废除，暗色判断
  // 一律走 isDarkTheme(id) 或 root.style.colorScheme，禁止再挂类）。
  root.dataset.theme = id;
  if (body) body.dataset.theme = id;
  const wantDark = isDarkTheme(id);
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

/* ── 强调色（L1）────────────────────────────────────────────────
   `--color-accent` 的供给链（见 tokens.css / base.css / dark.css 的注释）：

     base.css  :root          --color-accent: var(--color-accent-base)   ← 默认
     dark.css  :root[data-…]   --color-accent-base: <暗色值>              ← 主题层
     这里                          root.style.setProperty(…)                 ← 用户层

   关键：**移除属性**（`removeProperty`）而不是设成空串。CSS 的
   `var(x, fallback)` 只在 x 未被声明时才用 fallback；一旦声明为空，
   var() 返回空字符串，base.css 的供给规则会被架空成"没有颜色"。
   "跟随主题"必须是"该属性不存在"，不能是"该属性为空"。 */

// 只接受 `#rgb` / `#rrggbb` / `#rrggbbaa`。任何其它形状一律当作
// 无选择——把未经校验的字符串塞进 setProperty 就能注入任意 CSS。
const HEX_RE = /^#(?:[0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

function normalizeAccent(raw: string | null | undefined): string | null {
  if (!raw) return null;
  const v = raw.trim();
  return HEX_RE.test(v) ? v.toLowerCase() : null;
}

function applyAccent(color: string | null) {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (color) {
    root.style.setProperty("--color-accent", color);
  } else {
    root.style.removeProperty("--color-accent");
  }
}

function readStoredAccent(): string | null {
  try {
    return normalizeAccent(storage.getItem(STORAGE_KEYS.accentColor));
  } catch {
    return null;
  }
}

export function useTheme() {
  const [mode, setModeState] = useState<ThemeMode>(() => readStored());
  const [accent, setAccentState] = useState<string | null>(() =>
    readStoredAccent(),
  );

  useEffect(() => {
    applyTheme(mode);
    try { storage.setItem(STORAGE_KEYS.theme, mode); } catch {}
  }, [mode]);

  useEffect(() => {
    applyAccent(accent);
    try {
      if (accent) storage.setItem(STORAGE_KEYS.accentColor, accent);
      else storage.removeItem(STORAGE_KEYS.accentColor);
    } catch {}
  }, [accent]);

  const setMode = useCallback((next: ThemeMode) => setModeState(next), []);
  const cycle = useCallback(() => {
    setModeState((cur) => {
      const idx = THEMES.findIndex((t) => t.id === cur);
      return THEMES[(idx + 1) % THEMES.length]?.id ?? "light";
    });
  }, []);
  /** `null` 回到"跟随主题"。非 hex 形状会被 normalizeAccent 忽略。 */
  const setAccent = useCallback((next: string | null) => {
    setAccentState(normalizeAccent(next));
  }, []);
  return { mode, setMode, cycle, accent, setAccent };
}
