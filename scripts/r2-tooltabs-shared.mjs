// R2：把 ToolTabs 的共享 UI 件（SearchBar…SkeletonGrid）抽到 toolTabsShared.tsx
import { readFileSync, writeFileSync } from "node:fs";
const p = "<repo>/apps/desktop-tauri/src/components/config/ToolTabs.tsx";
const raw = readFileSync(p, "utf8");
const eol = raw.includes("\r\n") ? "\r\n" : "\n";
const lines = raw.split(eol);
const at = (i) => (lines[i] ?? "<eof>").replace(/\r$/, "").trim();

// 断言（1-based 1041/1177 → 0-based 1040/1176）
if (!at(1040).startsWith("export function SearchBar({")) { console.error("ABORT: SearchBar start mismatch:", JSON.stringify(at(1040))); process.exit(1); }
if (!at(1176).startsWith("function PluginCard({")) { console.error("ABORT: PluginCard marker mismatch:", JSON.stringify(at(1176))); process.exit(1); }
if (at(1174) !== "}") { console.error("ABORT: SkeletonGrid end mismatch:", JSON.stringify(at(1174))); process.exit(1); }

const block = lines.slice(1040, 1176)   // SearchBar…SkeletonGrid + 尾部空行
  .map((l) => l.replace(/^function /, "export function "))
  .map((l) => l.replace(/React\.ReactNode/g, "ReactNode"));

const shared = [
  "// toolTabsShared.tsx —— ToolTabs 四个 Tab 的共享 UI 件（R2-4 拆分第二步）。",
  "// 从 54KB God Object 抽出：SearchBar / SummaryBar / Section / Grid / EmptyHint / SkeletonGrid。",
  'import type { ReactNode } from "react";',
  'import { Search } from "lucide-react";',
  'import type { LucideIcon } from "lucide-react";',
  "",
  ...block,
].join(eol);
writeFileSync("<repo>/apps/desktop-tauri/src/components/config/toolTabsShared.tsx", shared, "utf8");

// ToolTabs：删除块 + 补导入
lines.splice(1040, 1176 - 1040);
const impIdx = lines.findIndex((l) => l.includes('from "./toolTabsData"'));
if (impIdx < 0) { console.error("ABORT: toolTabsData import not found"); process.exit(1); }
lines.splice(impIdx + 1, 0, 'import { SearchBar, SummaryBar, Section, Grid, EmptyHint, SkeletonGrid } from "./toolTabsShared";');
writeFileSync(p, lines.join(eol), "utf8");
console.log("extracted", block.length, "lines -> toolTabsShared.tsx; ToolTabs now", lines.length, "lines");
