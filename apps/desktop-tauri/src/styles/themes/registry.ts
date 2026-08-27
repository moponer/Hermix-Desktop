/**
 * Theme Registry -- auto-discovery.
 * User-dev new theme = drop <id>.css + <id>.json into themes/.
 * Vite import.meta.glob pulls them in at build time; they show up
 * automatically in the ThemeToggle dropdown.
 *
 * 主题契约：<id>.json 必填字段 id / label / colorScheme("light"|"dark")。
 * colorScheme 是唯一暗色判定来源 —— useTheme 据此设置 .dark 类与
 * colorScheme，禁止再在任何组件里硬编码主题名单。
 */
import type { ThemeMode } from "../../hooks/useTheme";

import "./light.css";
import "./dark.css";
import "./rainforest.css";
import "./sunset.css";

const cssModules = import.meta.glob("./*.css", { eager: true }) as Record<string, unknown>;
const jsonModules = import.meta.glob("./*.json", { eager: true, import: "default" }) as Record<string, unknown>;

export interface ThemeMeta {
  id: ThemeMode;
  label: string;
  description?: string;
  builtin?: boolean;
  /** 原生控件/滚动条/ECharts 渲染模式。新主题必填，缺省视为 light。 */
  colorScheme?: "light" | "dark";
}

function idFromPath(p: string): string {
  return p.replace(/^\.\//, "").replace(/\.(css|json)$/, "");
}

export const THEMES: ThemeMeta[] = (Object.keys(cssModules) as string[]).map((k) => {
  const id = idFromPath(k) as ThemeMode;
  const metaPath = `./${id}.json`;
  const meta = jsonModules[metaPath] as Partial<ThemeMeta> | undefined;
  return {
    id,
    label: meta?.label ?? id,
    description: meta?.description,
    builtin: meta?.builtin,
    colorScheme: meta?.colorScheme === "dark" ? ("dark" as const) : ("light" as const),
  };
}).sort((a, b) => {
  if (!!a.builtin !== !!b.builtin) return a.builtin ? -1 : 1;
  return a.label.localeCompare(b.label, "zh");
});

export function isKnownTheme(id: string): id is ThemeMode {
  return THEMES.some((t) => t.id === id);
}

/** 暗色族判定 —— 组件（如 ECharts 初始化）统一从这里问，不许自建名单。 */
export function isDarkTheme(id: string | undefined | null): boolean {
  const t = THEMES.find((t) => t.id === id);
  return t ? t.colorScheme === "dark" : false;
}
