// R2 第四步之一：ConnectorsTab + ConnectorRow → ConnectorsTab.tsx
import { readFileSync, writeFileSync } from "node:fs";
const p = "f:/<workspace>/hermix-desktop/apps/desktop-tauri/src/components/config/ToolTabs.tsx";
const out = "f:/<workspace>/hermix-desktop/apps/desktop-tauri/src/components/config/ConnectorsTab.tsx";
const raw = readFileSync(p, "utf8");
const eol = raw.includes("\r\n") ? "\r\n" : "\n";
const lines = raw.split(eol);
const at = (i) => (lines[i] ?? "<eof>").replace(/\r$/, "").trim();

if (!at(289).startsWith("// ── Connectors tab")) { console.error("ABORT start:", JSON.stringify(at(289))); process.exit(1); }
if (!at(291).startsWith("export function ConnectorsTab(")) { console.error("ABORT fn:", JSON.stringify(at(291))); process.exit(1); }
if (!at(540).startsWith("function ConnectorRow({")) { console.error("ABORT row:", JSON.stringify(at(540))); process.exit(1); }
const nextIdx = lines.findIndex((l, i) => i > 292 && l.replace(/\r$/, "").trim().startsWith("// ── Toolsets tab"));
if (nextIdx < 0) { console.error("ABORT: Toolsets marker not found"); process.exit(1); }

const block = lines.slice(289, nextIdx); // 注释 + ConnectorsTab + ConnectorRow（到 Toolsets 注释前）
const shared = [
  "// ConnectorsTab.tsx —— 连接器 Tab：用户配置的 MCP servers（R2-4 拆分第四步）。",
  'import { useCallback, useEffect, useMemo, useState } from "react";',
  'import { Plus, Trash2, X, Network, RefreshCw, ArrowUpRight, Wand2, Search } from "lucide-react";',
  'import * as api from "../../api";',
  'import { parseMcpJson, pickIconByName } from "./toolTabsData";',
  'import type { McpSpec } from "./toolTabsData";',
  'import { SearchBar, Section, Grid, EmptyHint, SkeletonGrid, SummaryBar } from "./toolTabsShared";',
  "",
  ...block,
].join(eol);
writeFileSync(out, shared, "utf8");

lines.splice(289, 588 - 289);
const impIdx = lines.findIndex((l) => l.includes('from "./SkillCard"'));
if (impIdx < 0) { console.error("ABORT: SkillCard import not found"); process.exit(1); }
lines.splice(impIdx + 1, 0, 'export { ConnectorsTab } from "./ConnectorsTab";');
writeFileSync(p, lines.join(eol), "utf8");
console.log("moved", block.length, "lines -> ConnectorsTab.tsx; ToolTabs now", lines.length, "lines");
