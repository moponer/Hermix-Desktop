#!/usr/bin/env node
// Theme contract guard — 三项静态检查（一票失败制，检查3的债项部分仅告警）：
//  1) 主题完整性：每套非 light 主题的 css 必须覆盖 CONTRACT 色彩令牌全集
//     （CONTRACT 从 tokens.css 的 @theme 块自动派生：--color-*/--theme-bg-*/--shadow-*；
//      字号/间距/圆角/动效是结构性令牌，跨主题恒定，不在契约内、禁止主题覆盖）。
//  2) 孤儿引用：任何 var(--x) 引用的变量必须在样式体系里有定义
//     （允许清单内的运行时局部变量除外，见 LOCAL_ALLOWED）。
//  3) 组件裸色：tsx/ts 内联字面色值不允许（例外文件见 EXCEPTIONS.md）；
//     Tailwind 原生调色板类当前为已登记债项 —— 仅统计告警不阻断，
//     迁移完毕后把 WARN_ONLY 改为 false 收紧。
import { readdirSync, readFileSync, statSync, existsSync } from "node:fs";
import { join, dirname, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const SRC = join(ROOT, "apps/desktop-tauri/src");
const STYLES = join(SRC, "styles");
const EXC = join(STYLES, "themes/EXCEPTIONS.md");

const read = (p) => readFileSync(p, "utf8");

/* ---------- Check 1: 每主题契约完整性 ---------- */
// 契约从 tokens.css @theme 块派生；别名层（:root 非 @theme 的 var(--x)→
// 别名映射）不在主题 css 内重复声明，导出自兼容块即可。
const ALIAS_NAMES = new Set([
  "--color-error-bg",
  "--color-warning-bg",
  "--color-error-text",
  "--color-text-success",
  "--font-family-primary",
]);
const tokensCss = read(join(STYLES, "tokens.css"));
const themeBlock = tokensCss.match(/@theme\s*\{([\s\S]*)\n\}/)?.[1] ?? "";
const CONTRACT = [...new Set(
  [...themeBlock.matchAll(/(--(?:color|theme-bg|shadow)-[a-z0-9-]+)\s*:/g)]
    .map((m) => m[1])
    .filter((n) => !ALIAS_NAMES.has(n))
)].sort();
if (CONTRACT.length === 0) {
  console.error("[check-theme-tokens] 无法从 tokens.css 解析 @theme 契约 —— 结构被改动？");
  process.exit(1);
}

let failures = 0;
for (const entry of readdirSync(join(STYLES, "themes"))) {
  if (!entry.endsWith(".css")) continue;
  const id = entry.replace(/\.css$/, "");
  if (id === "light") continue; // light = 默认层本身
  const defined = new Set([...read(join(STYLES, "themes", entry)).matchAll(/(--[a-z][a-z0-9-]*)\s*:/g)].map((m) => m[1]));
  const missing = CONTRACT.filter((t) => !defined.has(t));
  if (missing.length) {
    failures++;
    console.error(`[contract] ${id} 缺 ${missing.length} 个色彩令牌: ${missing.join(" ")}`);
  } else {
    console.log(`[contract] ${id}: ${CONTRACT.length}/${CONTRACT.length} ✓`);
  }
}

/* ---------- Check 2: 孤儿变量引用 ---------- */
const cssFiles = [];
(function scanStyles(d) {
  for (const e of readdirSync(d)) {
    const p = join(d, e);
    if (statSync(p).isDirectory()) scanStyles(p);
    else if (/\.css$/.test(e)) cssFiles.push(p);
  }
})(STYLES);

let anyDefined = new Set();
for (const f of cssFiles)
  for (const n of [...read(f).matchAll(/(--[a-z][a-z0-9-]*)\s*:/g)].map((m) => m[1]))
    anyDefined.add(n);

let usedVars = new Set();
for (const f of cssFiles)
  for (const m of read(f).matchAll(/var\(\s*(--[a-z0-9-]+)/g)) {
    // 过滤 `var(--color-${x})` 这类模板拼接产生的非完整名
    if (m[1].endsWith("-") || m[1] === "--color") continue;
    usedVars.add(m[1]);
  }
(function scanTsx(d) {
  for (const e of readdirSync(d)) {
    const p = join(d, e);
    if (statSync(p).isDirectory()) {
      if (!e.startsWith("themes")) scanTsx(p); // 主题目录自身是定义方
    } else if (/\.tsx?$/.test(e) && !e.includes(".test.")) {
      for (const m of read(p).matchAll(/var\(\s*(--[a-z0-9-]+)/g)) {
        // 过滤 `var(--color-${x})` 这类模板拼接产生的非完整名（裸 "--color-"）
        if (m[1].endsWith("-") || m[1] === "--color") continue;
        usedVars.add(m[1]);
      }
    }
  }
})(SRC);

// 运行时由 JS/grid 库写入的组件局部布局变量（合法非令牌）
const LOCAL_ALLOWED = new Set([
  "--ab-left-w", "--ab-right-w", "--pane-main-min", "--left-min",
  "--col-left", "--col-right",
]);
const orphans = [...usedVars]
  .filter((u) => !anyDefined.has(u) && !LOCAL_ALLOWED.has(u))
  .sort();
if (orphans.length) {
  failures++;
  console.error(`[orphan] ${orphans.length} 个被引用但未定义的变量: ${orphans.join(" ")}`);
} else {
  console.log("[orphan] 无孤儿引用 ✓");
}

/* ---------- Check 3: 组件裸色（阻断）+ Tailwind 原生色阶类（债项告警） ---------- */
function walkTsx(d) {
  const out = [];
  for (const entry of readdirSync(d)) {
    const p = join(d, entry);
    if (statSync(p).isDirectory()) out.push(...walkTsx(p));
    else if (/\.(tsx|ts)$/.test(entry) && !entry.includes(".test.")) out.push(p);
  }
  return out;
}

const allowedFiles = new Set();
if (existsSync(EXC)) {
  for (const line of read(EXC).split("\n")) {
    const m = line.match(/\|\s*(E\d+)\s*\|\s*`([^`]+)`/);
    if (!m) continue;
    allowedFiles.add(m[2].replace(/^apps\/desktop-tauri\/src\//, ""));
  }
}

const COLOR_RX = /(#[0-9a-fA-F]{3,8}\b|rgba?\s*\()/g;
const TOKEN_CTX = /var\(--color-[a-z0-9-]+/;
const violations = [];
for (const file of walkTsx(SRC)) {
  const rel = relative(SRC, file).split(sep).join("/");
  if (allowedFiles.has(rel)) continue;
  const lines = read(file).split("\n");
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    if (/^\s*(\/\/|\*|\/\*)/.test(ln)) continue;
    if (!COLOR_RX.test(ln)) continue;
    if (TOKEN_CTX.test(ln)) continue;
    violations.push(`${rel}:${i + 1}: ${ln.trim()}`);
  }
}
if (violations.length) {
  failures++;
  console.error("[bare-color] 组件内联硬编码色值:");
  for (const v of violations) console.error("  " + v);
  console.error("  → 改用语义令牌，或登记 EXCEPTIONS.md。");
} else {
  console.log("[bare-color] 无裸色 ✓");
}

const PALETTE_RX =
  /\b(text|bg|border)-(white|black|slate|zinc|neutral|stone|gray|red|orange|amber|yellow|green|emerald|teal|sky|blue|indigo|violet|purple|rose)-[0-9]{2,3}\b/g;
let paletteHits = 0;
for (const file of walkTsx(SRC)) {
  paletteHits += (read(file).match(PALETTE_RX) || []).length;
}
if (paletteHits > 0) {
  console.warn(
    `[palette-debt] ${paletteHits} 处 Tailwind 原生色阶类（已登记债项，仅告警）— 目标：迁移到语义令牌后在脚本中置 WARN_ONLY=false`,
  );
}

/* 字号债项：任意值 text-[Npx] 取代语义档位。
   历史包袱约 409 处（text-[11px] 194 / text-[13px] 104 / text-[12px] 93），
   根因是旧档位写成 v3 的 --font-size-* 命名，Tailwind v4 下不生成任何工具类，
   于是整套档位形同虚设。档位已改名到 --text-* 命名空间（tokens.css），
   迁移可分批做，故先 WARN_ONLY。 */
const TYPE_RX = /\btext-\[\s*\d+(?:\.\d+)?px\s*\]/g;
/* 品牌/资产类一次性尺寸（Logo、字体缩放刻度），见 tokens.css 几何令牌注释 */
const TYPE_ALLOWED = new Set([
  "text-[20px]", "text-[22px]", "text-[24px]", "text-[28px]", "text-[32px]",
  "text-[36px]", "text-[40px]", "text-[48px]", "text-[72px]",
]);
let typeHits = 0;
let typeFiles = 0;
for (const file of walkTsx(SRC)) {
  const hits = (read(file).match(TYPE_RX) || []).filter(
    (h) => !TYPE_ALLOWED.has(h.replace(/\s+/g, "")),
  );
  if (hits.length > 0) {
    typeHits += hits.length;
    typeFiles += 1;
  }
}
if (typeHits > 0) {
  console.warn(
    `[type-debt] ${typeHits} 处任意字号 text-[Npx]（分布于 ${typeFiles} 个文件，仅告警）— ` +
      `目标：全部迁到 text-micro/tiny/meta/ui/body/title/display 七档后在脚本中改为阻断`,
  );
}

process.exit(failures > 0 ? 1 : 0);
