import { useEffect, useState, useCallback } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";
import { THEMES, isKnownTheme, isDarkTheme } from "../styles/themes/registry";
import { onAccentFor } from "../styles/themes/accentPalette";

// 主题 = 任一已发现的 .css（内置仅 light / dark 两套；用户仍可自建 .css 放入 themes/）
// 数据源：themes/registry.ts 的 THEMES 数组 (auto-discover)
// 持久化键：hermix.desktop.theme
export type ThemeMode = string;

/** 跟随系统的哨兵值。不是主题 id，只表示"由 prefers-color-scheme 决定"。 */
export const SYSTEM_THEME = "system";

/** 主题切换后派发；非 React 集成点（ECharts 等三方实例）监听它重建。 */
export const THEME_CHANGED_EVENT = "hermix:theme-changed";

function systemPrefersDark(): boolean {
  if (typeof window === "undefined" || !window.matchMedia) return false;
  return window.matchMedia("(prefers-color-scheme: dark)").matches;
}

/**
 * 把用户选择解析成真正要写进 `data-theme` 的具体主题 id。
 *
 * "跟随系统"必须在这里塌缩成 light/dark 之一，而不是把 `system` 写进
 * `data-theme`——registry 里没有这个主题，写进去等于所有语义 token 全部
 * 取不到值（`data-theme` 是主题唯一来源，见 applyTheme 注释）。
 */
export function resolveTheme(mode: ThemeMode): string {
  if (mode !== SYSTEM_THEME) return mode;
  return systemPrefersDark() ? "dark" : "light";
}

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
    if (v === SYSTEM_THEME) return SYSTEM_THEME;
    if (v && isKnownTheme(v)) return v;
  } catch {}
  return "light";
}

/* ── 强调色（L1）────────────────────────────────────────────────
   `--color-accent` 的供给链（见 tokens.css / base.css / dark.css 的注释）：

     tokens.css @theme          --color-accent: <亮色字面量>              ← 主题层（默认）
     dark.css  :root[data-…]    --color-accent: <暗色字面量>              ← 主题层（暗色）
     这里                          root.style.setProperty(…)                 ← 用户层

   2026-10-04：原链路多一跳——tokens.css 只给 `--color-accent-base`，由
   base.css 的一条接桥声明转交给 `--color-accent`。该拆分使
   `--color-accent` 落在 @theme 之外，Tailwind v4 因此不生成
   bg-accent / text-accent / border-accent，117 处调用静默失效。已合并为
   两层，行内注入的优先级不受影响（行内 > :root[data-theme] > :root）。

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
    // on-accent 必须与 accent 一起注入：它无法由 color-mix 推导（见
    // accentPalette.ts 的说明）。两个变量成对设、成对移除，否则"跟随主题"
    // 态会残留一个孤儿 on 值。
    root.style.setProperty("--color-on-accent", onAccentFor(color));
  } else {
    root.style.removeProperty("--color-accent");
    root.style.removeProperty("--color-on-accent");
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
  /** 实际生效的主题 id：mode 非 system 时等于 mode。 */
  const [resolved, setResolved] = useState<ThemeMode>(() => resolveTheme(readStored()));

  useEffect(() => {
    const apply = () => {
      const id = resolveTheme(mode);
      setResolved(id);
      applyTheme(id);
    };
    apply();
    try { storage.setItem(STORAGE_KEYS.theme, mode); } catch {}
    if (mode !== SYSTEM_THEME || typeof window === "undefined" || !window.matchMedia) {
      return;
    }
    // 跟随系统态下要挂在 OS 偏好变化上；显式选主题时不挂——用户既然
    // 指定了 light/dark，系统再改也不该把界面拽走。
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
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
      const resolvedCur = resolveTheme(cur);
      const idx = THEMES.findIndex((t) => t.id === resolvedCur);
      return THEMES[(idx + 1) % THEMES.length]?.id ?? "light";
    });
  }, []);
  /** `null` 回到"跟随主题"。非 hex 形状会被 normalizeAccent 忽略。 */
  const setAccent = useCallback((next: string | null) => {
    setAccentState(normalizeAccent(next));
  }, []);
  return { mode, resolved, setMode, cycle, accent, setAccent };
}
