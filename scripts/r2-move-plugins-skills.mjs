// R2 第四步之三/四：PluginsTab(+PluginCard) 与 SkillsTab 出走，ToolTabs 变 barrel
import { readFileSync, writeFileSync } from "node:fs";
const p = "f:/<workspace>/hermix-desktop/apps/desktop-tauri/src/components/config/ToolTabs.tsx";
const raw = readFileSync(p, "utf8");
const eol = raw.includes("\r\n") ? "\r\n" : "\n";
const lines = raw.split(eol);
const at = (i) => (lines[i] ?? "<eof>").replace(/\r$/, "").trim();

// 断言（0-based）
if (!at(23).startsWith("// ── Plugins tab")) { console.error("ABORT plugins-start:", JSON.stringify(at(23))); process.exit(1); }
if (!at(102).startsWith("// ── Skills tab")) { console.error("ABORT skills-start:", JSON.stringify(at(102))); process.exit(1); }
if (!at(279).startsWith("// ── SkillCard")) { console.error("ABORT skc:", JSON.stringify(at(279))); process.exit(1); }
if (!at(284).startsWith("// ── Shared building blocks")) { console.error("ABORT shared:", JSON.stringify(at(284))); process.exit(1); }

// SkillsTab 块：1-based 103..279（含注释头）→ 0-based 102..279
const skillsBlock = lines.slice(102, 280).map((l) => l.replace(/^export function /, "export function "));
// Plugins 块：1-based 24..102（PluginsTab）+ 1-based 285..end（PluginCard）
const pluginsBlock = [
  ...lines.slice(23, 102),
  "",
  ...lines.slice(284).map((l) => l.replace(/^function /, "export function ")),
];

const pluginsFile = [
  "// PluginsTab.tsx —— 内置能力开关 Tab + PluginCard（R2-4 拆分第六步）。",
  'import { useCallback, useState } from "react";',
  'import { Sparkles, RefreshCw, ArrowUpRight, Plus } from "lucide-react";',
  'import type { ReactNode } from "react";',
  'import * as api from "../../api";',
  'import type { PluginSettings } from "../../api";',
  'import { BUILTIN_PLUGINS, DEFAULT_PLUGIN_SETTINGS } from "./toolTabsData";',
  'import { ToolCardIcon } from "./ToolCardIcon";',
  'import { SearchBar, Section, Grid, EmptyHint, SkeletonGrid, SummaryBar } from "./toolTabsShared";',
  "",
  ...pluginsBlock,
].join(eol);
writeFileSync("f:/<workspace>/hermix-desktop/apps/desktop-tauri/src/components/config/PluginsTab.tsx", pluginsFile, "utf8");

const skillsFile = [
  "// SkillsTab.tsx —— 技能 Tab：GET /api/skills + 启停（R2-4 拆分第七步）。",
  'import { useCallback, useEffect, useMemo, useState } from "react";',
  'import { RefreshCw } from "lucide-react";',
  'import * as api from "../../api";',
  'import type { SkillInfo } from "../../api";',
  'import { CATEGORY_LABELS } from "./toolTabsData";',
  'import { SkillCard } from "./SkillCard";',
  'import { SearchBar, Section, Grid, EmptyHint, SkeletonGrid, SummaryBar } from "./toolTabsShared";',
  "",
  ...skillsBlock,
].join(eol);
writeFileSync("f:/<workspace>/hermix-desktop/apps/desktop-tauri/src/components/config/SkillsTab.tsx", skillsFile, "utf8");

// ToolTabs 变 barrel：保留头部注释 + 四个 re-export
const barrel = [
  "// ToolTabs.tsx —— 配置域四 Tab 的 barrel（R2-4 拆分完成后仅负责再导出）。",
  "// 实现分布：PluginsTab.tsx / SkillsTab.tsx / ConnectorsTab.tsx / ToolsetsTab.tsx，",
  "// 共享件在 toolTabsShared.tsx，常量与解析器在 toolTabsData.ts。",
  'export { PluginsTab } from "./PluginsTab";',
  'export { SkillsTab } from "./SkillsTab";',
  'export { ConnectorsTab } from "./ConnectorsTab";',
  'export { ToolsetsTab } from "./ToolsetsTab";',
  "",
].join(eol);
writeFileSync(p, barrel, "utf8");
console.log("done: Plugins", pluginsBlock.length, "lines / Skills", skillsBlock.length, "lines / ToolTabs -> barrel");
