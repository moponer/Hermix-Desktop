/**
 * Theme Registry -- auto-discovery.
 * User-dev new theme = drop <id>.css + <id>.json into themes/.
 * Vite import.meta.glob pulls them in at build time; they show up
 * automatically in the ThemeToggle dropdown.
 */
import type { ThemeMode } from "../../hooks/useTheme";

import "./light.css";
import "./dark.css";
import "./snow.css";
import "./rain.css";
import "./rainforest.css";

const cssModules = import.meta.glob("./*.css", { eager: true }) as Record<string, unknown>;
const jsonModules = import.meta.glob("./*.json", { eager: true, import: "default" }) as Record<string, unknown>;

export interface ThemeMeta {
  id: ThemeMode;
  label: string;
  description?: string;
  builtin?: boolean;
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
  };
}).sort((a, b) => {
  if (!!a.builtin !== !!b.builtin) return a.builtin ? -1 : 1;
  return a.label.localeCompare(b.label, "zh");
});

export function isKnownTheme(id: string): id is ThemeMode {
  return THEMES.some((t) => t.id === id);
}
