// R2 第四步之二：ToolsetsTab + ToolsetCard → ToolsetsTab.tsx
import { readFileSync, writeFileSync } from "node:fs";
const p = "<repo>/apps/desktop-tauri/src/components/config/ToolTabs.tsx";
const out = "<repo>/apps/desktop-tauri/src/components/config/ToolsetsTab.tsx";
const raw = readFileSync(p, "utf8");
const eol = raw.includes("\r\n") ? "\r\n" : "\n";
const lines = raw.split(eol);
const at = (i) => (lines[i] ?? "<eof>").replace(/\r$/, "").trim();

if (!at(285).startsWith("// ── Toolsets tab")) { console.error("ABORT start:", JSON.stringify(at(285))); process.exit(1); }
if (!at(287).startsWith("export function ToolsetsTab(")) { console.error("ABORT fn:", JSON.stringify(at(287))); process.exit(1); }
if (!at(479).startsWith("function ToolsetCard({")) { console.error("ABORT card:", JSON.stringify(at(479))); process.exit(1); }
if (!at(550).startsWith("// ── Shared building blocks")) { console.error("ABORT next:", JSON.stringify(at(550))); process.exit(1); }

const block = lines.slice(285, 550); // 注释 + ToolsetsTab + ToolsetCard
const shared = [
  "// ToolsetsTab.tsx —— 工具集 Tab：后端工具注册表分组（R2-4 拆分第五步）。",
  'import { useCallback, useEffect, useMemo, useState } from "react";',
  'import { Plus, Trash2, RefreshCw, Wand2, Search, Boxes, X } from "lucide-react";',
  'import * as api from "../../api";',
  'import type { ToolsetInfo } from "../../api";',
  'import { pickIconByName } from "./toolTabsData";',
  'import { ToolCardIcon } from "./ToolCardIcon";',
  'import { MgmtTag } from "./MgmtTag";',
  'import { ToggleSwitch } from "../ToggleSwitch";',
  'import { SearchBar, Section, Grid, EmptyHint, SkeletonGrid, SummaryBar } from "./toolTabsShared";',
  "",
  ...block,
].join(eol);
writeFileSync(out, shared, "utf8");

lines.splice(285, 550 - 285);
const impIdx = lines.findIndex((l) => l.includes('from "./ConnectorsTab"'));
if (impIdx < 0) { console.error("ABORT: barrel line not found"); process.exit(1); }
lines.splice(impIdx + 1, 0, 'export { ToolsetsTab } from "./ToolsetsTab";');
writeFileSync(p, lines.join(eol), "utf8");
console.log("moved", block.length, "lines -> ToolsetsTab.tsx; ToolTabs now", lines.length, "lines");
