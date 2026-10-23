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
//
// 2026-10-04：`--color-accent` 曾因「主题层 / 使用层」拆分而被列在此处
// （别名层），因此**不进契约**。它当时只由 base.css 的一条接桥声明供给，
// 主题对它确实没有决定权。但代价是 Tailwind v4 不生成任何 `*-accent`
// 工具类（accent 不在 @theme 颜色命名空间内），117 处调用静默失效。
//
// 现已合并：`--color-accent` 回到 @theme 内的主题层，themes/dark.css 覆写
// 同一个键。因此它**必须**进契约——若有人日后从 dark.css 里删掉这行，
// 门禁要能立刻报出来，而不是让暗色主题静默回落成亮色的松墨绿。
//
// accent 的四档派生（hover / on-text / soft / soft-strong）2026-10-04 一并
// 移入 @theme，好让 Tailwind 生成对应的工具类；但它们是对 --color-accent 的
// color-mix **派生**，主题对它们没有决定权（该混白还是混黑由派生式本身决定，
// 不是主题选出来的），因此同 --color-accent 合并前的处境一样，列入别名清单
// 不进契约。放 @theme 只是为了让工具类生成，与是否进契约无关。
const ALIAS_NAMES = new Set([
  "--color-error-bg",
  "--color-warning-bg",
  "--color-error-text",
  "--font-family-primary",
  "--color-accent-hover",
  "--color-accent-on-text",
  "--color-accent-soft",
  "--color-accent-soft-strong",
  // 派生档：由 --color-bg-tertiary 混白得到，**与主题无关**（每套主题解析出的
  // 值自动跟随它压的那张底），所以不进各主题的契约。
  "--color-code-inline-bg",
]);
// 注意：--color-text-success **不在**这个列表里 —— 它曾经是
// 「success 当文字用」的别名（tokens.css 兼容层里 var(--color-success)），
// 2026-10-21 起语义四色有了各自的文字安全档，它变成了**每套主题各给一个值**
// 的真令牌，必须进契约，否则新主题漏声明会静默继承亮色值。
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
  const content = read(join(STYLES, "themes", entry));
  /* Check 1a（2026-10-16）：主题选择器必须精确存在。选择器写坏（实测：
     dark.css 被写成双冒号 `::root[data-theme=…]`）会让整条规则被浏览器
     当非法选择器丢弃，暗色主题静默失效成浅色 —— 而本工具只数令牌数
     不出来，当时照样 103/103 全绿。 */
  if (!content.includes(`:root[data-theme="${id}"]`)) {
    failures++;
    console.error(`[contract] ${id}: 找不到主题选择器 :root[data-theme="${id}"] —— 整条规则会被浏览器丢弃（暗色静默失效）`);
    continue;
  }
  const defined = new Set([...content.matchAll(/(--[a-z][a-z0-9-]*)\s*:/g)].map((m) => m[1]));
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
// CSS 块注释先剥掉：注释里的示例写法（"曾经写成 var(--accent)"）会被
// var() 正则当成真实引用，报出幽灵孤儿变量。
const stripCssComments = (s) => s.replace(/\/\*[\s\S]*?\*\//g, "");
for (const f of cssFiles)
  for (const n of [...stripCssComments(read(f)).matchAll(/(--[a-z][a-z0-9-]*)\s*:/g)].map((m) => m[1]))
    anyDefined.add(n);

/* 共享包的 CSS 也是"样式体系"的一部分：desktop 端在 globals.css 里
   `@import "@hermix/preview/styles/icon-tone.css"`，那份文件定义的局部变量
   （如 .cat-icon 的 --cat-tone）在 desktop 侧同样可用。此前本检查只扫
   apps/desktop-tauri/src/styles，于是引用共享层变量的桌面样式会被误报成
   "孤儿引用" —— 而孤儿检查要抓的是**拼错的变量名**（var() 解析失败 =
   整条声明被丢弃），共享层定义的真变量不属于这一类。 */
const SHARED_STYLES = join(ROOT, "packages/preview/src/styles");
if (existsSync(SHARED_STYLES)) {
  for (const e of readdirSync(SHARED_STYLES)) {
    if (!e.endsWith(".css")) continue;
    for (const n of [...stripCssComments(read(join(SHARED_STYLES, e))).matchAll(/(--[a-z][a-z0-9-]*)\s*:/g)].map((m) => m[1]))
      anyDefined.add(n);
  }
}

let usedVars = new Set();
for (const f of cssFiles)
  for (const m of stripCssComments(read(f)).matchAll(/var\(\s*(--[a-z0-9-]+)/g)) {
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
      // 注释里的示例（"禁写 bg-[var(--accent)]"）不是真实引用，先剥掉
      const src = read(p).replace(/\/\*[\s\S]*?\*\//g, "").replace(/(^|\s)\/\/.*$/gm, "$1");
      for (const m of src.matchAll(/var\(\s*(--[a-z0-9-]+)/g)) {
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

// CSS 层同规。2026-10-18 扩围：此前只扫 tsx/ts，styles/** 约 370 处字面色值
// 与 130+ 处 px 字号永远报绿——TYPOGRAPHY_SYSTEM.md「已全部归到 token」的
// 结论与代码事实不符。存量较大，先 WARN 统计（见文件末汇总），逐文件迁移
// 收敛到零后改阻断。
const CSS_SCAN = cssFiles.filter((f) => !f.includes(`${sep}themes${sep}`));

const allowedFiles = new Set();
if (existsSync(EXC)) {
  for (const line of read(EXC).split("\n")) {
    // 表格第一列是文件路径（`src/...` 或 `apps/desktop-tauri/src/...`）。
    // 2026-10-18 修：旧正则要求 `| E编号 |` 列，而登记表从来没有编号列
    // ——allowedFiles 恒为空，ProviderIcon/FileTypeIcon 等"已登记"项
    // 实际上从未生效过（门禁一直红着没人发现，因为没人单独跑它）。
    const m = line.match(/^\|\s*`([^`]+)`/);
    if (!m) continue;
    const p = m[1]
      .replace(/^apps\/desktop-tauri\/src\//, "")
      .replace(/^src\//, "");
    allowedFiles.add(p);
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

/* ---------- Check 3b: CSS 层裸色（WARN，迁移收敛后改阻断） ---------- */
// 豁免规则收紧：旧版「行内含 var(--color- 即整行豁免」，于是
// `var(--color-error, #dc2626)` 这种兜底写法整行放行。新版把色值出现位置
// 全部限定在 var() 的兜底参数里——即去掉所有 var(...) 段之后不再有裸色。
function stripVarSegments(line) {
  return line.replace(/var\(\s*--[a-z0-9-]+\s*(?:,[^()]*)?\)/g, "");
}
const cssColorHits = [];
for (const file of CSS_SCAN) {
  const rel = relative(SRC, file).split(sep).join("/");
  if (allowedFiles.has(rel)) continue;
  const lines = read(file).split("\n");
  let inBlock = false;
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    // 跨行 /* … */ 注释状态机：注释中间行（不以 * 开头的中文行）也豁免。
    if (inBlock) {
      if (ln.includes("*/")) inBlock = false;
      continue;
    }
    const openIdx = ln.indexOf("/*");
    if (openIdx >= 0 && !ln.slice(0, openIdx).includes("*/")) {
      if (!ln.slice(openIdx).includes("*/")) inBlock = true;
      continue;
    }
    if (/^\s*(\/\/|\*|\/\*)/.test(ln)) continue;
    if (!COLOR_RX.test(ln)) continue;
    // 整行都是 var() 声明（定义侧，如 `--x: #fff;`）不算裸色消费
    if (/^\s*--[a-z0-9-]+\s*:/.test(ln)) continue;
    if (COLOR_RX.test(stripVarSegments(ln))) {
      cssColorHits.push(`${rel}:${i + 1}: ${ln.trim().slice(0, 100)}`);
    }
  }
}
if (cssColorHits.length) {
  console.warn(`[css-bare-color] ${cssColorHits.length} 处 CSS 层裸色（WARN，登记或迁移）:`);
  const byFile = new Map();
  for (const h of cssColorHits) {
    const f = h.split(":")[0];
    byFile.set(f, (byFile.get(f) ?? 0) + 1);
  }
  for (const [f, n] of [...byFile.entries()].sort((a, b) => b[1] - a[1])) {
    console.warn(`  ${f}: ${n} 处`);
  }
} else {
  console.log("[css-bare-color] CSS 层无裸色 ✓");
}

/* ---------- Check 3c: CSS 层 px 字号（WARN） ---------- */
const cssTypeRx = /font-size:\s*(\d+(?:\.\d+)?)px/g;
const cssTypeHits = [];
for (const file of CSS_SCAN) {
  const rel = relative(SRC, file).split(sep).join("/");
  if (allowedFiles.has(rel)) continue;
  const hits = [...read(file).matchAll(cssTypeRx)].map((m) => m[1]);
  if (hits.length) cssTypeHits.push(`  ${rel}: ${hits.length} 处（${[...new Set(hits)].join("/")}px）`);
}
if (cssTypeHits.length) {
  console.warn(`[css-type-debt] CSS 层 px 字号（WARN，改用 --text-* 七档）:`);
  for (const h of cssTypeHits) console.warn(h);
} else {
  console.log("[css-type-debt] CSS 层无 px 字号 ✓");
}

/* ---------- Check 3d: CSS 层暗色字面量兜底（HARD） ----------
   `var(--x, #1e293b)` 形态把「暗色」写死为默认值：令牌缺失时静默
   变深色，排查困难（themes/EXCEPTIONS.md 纪律 2 同源问题）。
   2026-10-18 存量 237 处已全量清除（一次性脚本验证变量确有定义后
   变量确有定义才删）；此后出现即为新增债，直接 fail。 */
const fallbackRx = /var\(--[a-zA-Z0-9-]+,\s*(#[0-9a-fA-F]{3,8}|rgba?\()/g;
const fallbackHits = [];
for (const file of CSS_SCAN) {
  const rel = relative(SRC, file).split(sep).join("/");
  if (allowedFiles.has(rel)) continue;
  const n = [...read(file).matchAll(fallbackRx)].length;
  if (n) fallbackHits.push(`  ${rel}: ${n} 处`);
}
if (fallbackHits.length) {
  console.error(`[css-fallback-debt] 暗色字面量兜底禁止（删兜底值，令牌已在 tokens.css 定义）:`);
  for (const h of fallbackHits) console.error(h);
  process.exitCode = 1;
} else {
  console.log("[css-fallback-debt] CSS 层无暗色兜底 ✓");
}

/* ---------- Check 4: Tailwind 原生色阶类 ----------
   L0（2026-10-01）：在用的 21 个色槽已在 tokens.css 的 @theme 内
   重定向到语义令牌（error / warning / success / 文本灰阶），所以
   这些类名**是合法的**——它们解析后跟随主题与强调色。

   门禁现在拦的是**未登记的色阶类**：用了 tokens.css 里没有对应槽的
   `text-red-300` / `bg-sky-500` 之类，那类会静默解析成 Tailwind
   默认色板值，完全不跟随主题，属于新的色债。

   换槽（即往 PALETTE_ALIASES 加一行）是受控的扩展点：加进去之前
   必须在 tokens.css @theme 和每套主题 css 里都声明，否则 CONTRACT
   检查会失败。 */
/** 已在 tokens.css @theme 重定向到语义令牌的槽。与 tokens.css 里那
 *  21 行一一对应——改这里就要同步改 tokens.css 和各主题 css。 */
const PALETTE_ALIASES = [
  "red-200", "red-300", "red-400", "red-500",
  "rose-200", "rose-300", "rose-500",
  "amber-200", "amber-300", "amber-400", "amber-500",
  "yellow-400",
  "green-400", "green-500",
  "emerald-200", "emerald-300", "emerald-400", "emerald-500",
  "zinc-300", "zinc-400", "zinc-500",
];
const PALETTE_RX =
  /\b(?:text|bg|border)-(white|black|slate|zinc|neutral|stone|gray|red|orange|amber|yellow|green|emerald|teal|sky|blue|indigo|violet|purple|rose)-[0-9]{2,3}\b/g;
/** 已在 tokens.css @theme 重定向到语义令牌的槽（与 PALETTE_ALIASES 同源）。 */
const PALETTE_ALLOWED = new Set(PALETTE_ALIASES);
const paletteHits = [];
for (const file of walkTsx(SRC)) {
  const rel = relative(SRC, file).split(sep).join("/");
  for (const m of read(file).matchAll(PALETTE_RX)) {
    const slot = m[0].replace(/^(?:text|bg|border)-/, "");
    if (PALETTE_ALLOWED.has(slot)) continue; // 合法：已重定向
    paletteHits.push(`${rel}: ${m[0]}`);
  }
}
if (paletteHits.length > 0) {
  failures++;
  console.error(
    `[palette-debt] ${paletteHits.length} 处未登记的 Tailwind 原生色阶类（会解析为默认色板，不跟随主题）:`,
  );
  for (const h of paletteHits) console.error(`  ${h}`);
  console.error(
    "  → 改用语义令牌类，或在 tokens.css @theme + 各主题 css 登记该槽后加进 PALETTE_ALIASES。",
  );
} else {
  console.log("[palette-debt] 无未登记色阶类 ✓（在用槽已全部重定向到语义令牌）");
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
  // 2026-10-01：13 处债已清零（LocalSearchDialog 11 / SessionPickerDialog 2，
  // 全部落在既有七档内：11/11.5→tiny、12/12.5→meta、13→ui），门禁由告警
  // 改为阻断。任意值字号会让整套档位形同虚设——档位存在却不许用，
  // 等于没有档位。
  failures++;
  console.error(
    `[type-debt] ${typeHits} 处任意字号 text-[Npx]（分布于 ${typeFiles} 个文件）:`,
  );
  for (const f of walkTsx(SRC)) {
    const hits = (read(f).match(TYPE_RX) || []).filter(
      (h) => !TYPE_ALLOWED.has(h.replace(/\s+/g, "")),
    );
    if (hits.length > 0) {
      console.error(
        `  ${relative(SRC, f).split(sep).join("/")}: ${[...new Set(hits)].join(" ")}`,
      );
    }
  }
  console.error(
    "  → 改用七档之一：text-micro(10) / tiny(11) / meta(12) / ui(13) / " +
      "body(14) / title(16) / display(20)。半档（12.5px）就近取档——" +
      "一屏内字号档位 ≤ 3 是 tokens.css 的约定。",
  );
} else {
  console.log("[type-debt] 无任意字号 ✓（全部使用七档语义档位）");
}

/* ════════════════════════════════════════════════════════════════════
 * Check 6 · contrast —— 前景/背景配对的 WCAG 对比度实测
 * ────────────────────────────────────────────────────────────────────
 * 契约校验只保证「三套主题都声明了同一批令牌」，**不保证这些取值可用**。
 * 2026-10-21 审计实测出 8 处 FAIL，最严重的一处是 ink-green 的
 * --color-text-hero: #ffffff 压在 #fdfefd 的纸面上 —— 对比度 1.01:1，
 * 欢迎页标题完全看不见（注释还写着「暗底主题用白」，是从 dark 复制过来
 * 忘了 ink-green 是浅色主题）。
 *
 * 这条检查把「取值必须真的能读」变成机械断言，而不是靠人眼发现。
 * 背景色的 alpha 要**合成**到纸面上再算——半透明底的观感取决于它压在什么上。
 * ════════════════════════════════════════════════════════════════════ */
{
  const hexRgb = (v) => {
    const m = /^#([0-9a-f]{6})$/i.exec((v || "").trim());
    if (!m) return null;
    const x = m[1];
    return [parseInt(x.slice(0, 2), 16), parseInt(x.slice(2, 4), 16), parseInt(x.slice(4, 6), 16)];
  };
  const lum = ([r, g, b]) => {
    const f = (c) => { c /= 255; return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4); };
    return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
  };
  const ratio = (a, b) => {
    if (!a || !b) return null;
    const l1 = lum(a), l2 = lum(b);
    return (Math.max(l1, l2) + 0.05) / (Math.min(l1, l2) + 0.05);
  };
  /** 拆出尾部百分比：「var(--x) 14%」→ {expr:"var(--x)", pct:14}。 */
  const splitPct = (v) => {
    const m = /^(.*?)\s*([\d.]+)%\s*$/.exec(String(v || "").trim());
    if (!m) return { expr: String(v || "").trim(), pct: null };
    return { expr: m[1].trim(), pct: Number(m[2]) };
  };
  /**
   * 解析颜色表达式 → [r,g,b,a]。
   * 支持 #rrggbb / rgb() / rgba() / transparent / var(--x) 递归 /
   * color-mix(in srgb, A p%, B p%)。
   *
   * 2026-10-21 补 color-mix：此前只认 hex 与 rgba()，于是所有以
   * color-mix 派生的底色（accent-soft 两档）会被**静默跳过**——
   * 门禁显示全绿，实际根本没测。加了这条能力后新配对才真的被测到。
   */
  const parseColor = (value, vars, depth = 0) => {
    if (value === null || value === undefined || depth > 8) return null;
    const v = String(value).trim();
    if (!v) return null;
    const hex = hexRgb(v);
    if (hex) return [hex[0], hex[1], hex[2], 1];
    if (/^transparent$/i.test(v)) return [0, 0, 0, 0];
    // 具名色：只认 color-mix 里实际用到的这四个，其余返回 null（→ 报 WARN）
    if (/^white$/i.test(v)) return [255, 255, 255, 1];
    if (/^black$/i.test(v)) return [0, 0, 0, 1];
    const m = /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+)\s*)?\)$/i.exec(v);
    if (m) return [Number(m[1]), Number(m[2]), Number(m[3]), m[4] === undefined ? 1 : Number(m[4])];
    const vm = /^var\(\s*--([a-z0-9-]+)\s*\)$/i.exec(v);
    if (vm) return parseColor(vars[vm[1]], vars, depth + 1);
    const cm = /^color-mix\(\s*in\s+srgb\s*,\s*(.+?)\s*,\s*(.+?)\s*\)$/i.exec(v);
    if (cm) {
      const pa = splitPct(cm[1]);
      const pb = splitPct(cm[2]);
      const a = parseColor(pa.expr, vars, depth + 1);
      const b = parseColor(pb.expr, vars, depth + 1);
      if (!a || !b) return null;
      // 一端给了百分比则另一端自动补足；两端都没给 → 各 50%
      const t = pa.pct !== null || pb.pct !== null
        ? Math.min(1, Math.max(0, (pa.pct ?? 0) / 100))
        : 0.5;
      return [
        a[0] * t + b[0] * (1 - t),
        a[1] * t + b[1] * (1 - t),
        a[2] * t + b[2] * (1 - t),
        a[3] * t + b[3] * (1 - t),
      ];
    }
    return null;
  };
  /** 把半透明底合成到 over 上，返回实色。 */
  const resolveBg = (value, vars) => {
    const c = parseColor(value, vars);
    if (!c) return null;
    if (c[3] >= 1) return [c[0], c[1], c[2]];
    const over = hexRgb(vars["color-bg-primary"]);
    if (!over) return null;
    return [
      c[0] * c[3] + over[0] * (1 - c[3]),
      c[1] * c[3] + over[1] * (1 - c[3]),
      c[2] * c[3] + over[2] * (1 - c[3]),
    ];
  };

  const themeValues = (id) => {
    if (id === "light") return parseThemeVars(themeBlock);
    const file = join(STYLES, `themes/${id}.css`);
    if (!existsSync(file)) return null;
    return parseThemeVars(stripCssComments(read(file)));
  };
  const parseThemeVars = (css) => {
    const out = {};
    const re = /--([a-z0-9-]+)\s*:\s*([^;]+);/gi;
    let m;
    while ((m = re.exec(css))) out[m[1]] = m[2].trim();
    return out;
  };

  // [前景, 背景, 最低对比度, 说明]
  // 4.5 = 正文级（WCAG 1.4.3 AA）；3.0 = 控件边界等非文本（1.4.11）
  const CONTRAST_PAIRS = [
    ["color-text-primary", "color-bg-primary", 4.5, "正文 / 纸面"],
    ["color-text-primary", "color-bg-card", 4.5, "正文 / 卡片"],
    ["color-text-primary", "color-bg-secondary", 4.5, "正文 / 侧栏案面"],
    ["color-text-primary", "color-bg-tertiary", 4.5, "正文 / chip 底"],
    ["color-text-secondary", "color-bg-primary", 4.5, "次级 / 纸面"],
    ["color-text-secondary", "color-bg-card", 4.5, "次级 / 卡片"],
    ["color-text-secondary", "color-bg-tertiary", 4.5, "次级 / chip 底"],
    ["color-text-tertiary", "color-bg-primary", 4.5, "三级 / 纸面"],
    ["color-text-tertiary", "color-bg-card", 4.5, "三级 / 卡片"],
    ["color-text-faint", "color-bg-primary", 4.5, "弱化 / 纸面"],
    ["color-text-faint", "color-bg-secondary", 4.5, "弱化 / 侧栏"],
    ["color-text-thinking", "color-bg-primary", 4.5, "思考 / 纸面"],
    ["color-text-thinking", "color-bg-card", 4.5, "思考 / 卡片"],
    ["color-text-hero", "color-bg-primary", 4.5, "hero 标题 / 纸面"],
    ["color-link", "color-bg-primary", 4.5, "链接 / 纸面"],
    ["color-link", "color-bg-tertiary", 4.5, "链接 / chip 底"],
    ["color-link-soft", "color-bg-primary", 4.5, "弱链接 / 纸面"],
    ["color-text-success", "color-bg-primary", 4.5, "success 文字 / 纸面"],
    ["color-text-success", "color-success-soft", 4.5, "success 文字 / success-soft 底"],
    ["color-text-warning", "color-bg-primary", 4.5, "warning 文字 / 纸面"],
    ["color-text-warning", "color-warning-soft", 4.5, "warning 文字 / warning-soft 底"],
    ["color-text-error", "color-bg-primary", 4.5, "error 文字 / 纸面"],
    ["color-text-error", "color-error-soft", 4.5, "error 文字 / error-soft 底"],
    ["color-text-info", "color-bg-primary", 4.5, "info 文字 / 纸面"],
    ["color-text-info", "color-info-soft", 4.5, "info 文字 / info-soft 底"],
    ["color-on-accent", "color-accent", 4.5, "accent 底上的文字"],
    // 实心色块上的字形（共享层 .cat-tile，2026-10-23）：字形前景是
    // --color-on-tone，底是 8 档身份色之一。字形是**图形**不是正文，
    // 按 WCAG 1.4.11 取 3:1（不是文字的 4.5:1）。
    // 这 8 组 × 3 主题是本轮新增的硬约束：把 --color-on-tone 写成死值
    // （例如全用 #ffffff）会在深色主题下静默掉到 2.0~2.6:1 —— 门禁要
    // 能在那一刻报出来，而不是等肉眼发现"暗色下图标看不清了"。
    ...["slate", "blue", "cyan", "green", "amber", "orange", "rose", "violet"].map(
      (t) => [`color-on-tone`, `color-tone-${t}`, 3.0, `实心色块字形 / ${t} 底（1.4.11 的 3:1）`],
    ),
    ["color-text-on-code", "color-bg-code", 4.5, "代码正文 / 代码底"],
    ["color-border", "color-bg-primary", 3.0, "控件边界 / 纸面（1.4.11 的 3:1）"],
    ["color-border", "color-bg-card", 3.0, "控件边界 / 卡片（1.4.11 的 3:1）"],
    // 发丝分隔线的真实契约是「与所压底色的亮度比 ≥1.10」（tokens.css 的
    // 分隔线一节），**不是** 3:1 —— 1.4.11 的 3:1 落在 --color-border 上。
    // 2026-10-21 起 --color-border-secondary 的注释曾误写「WCAG 1.4.11 3:1」，
    // 实测只有 1.37–1.70:1；注释已改正，规则也按真实契约判。
    ["color-border-secondary", "color-bg-primary", 1.1, "发丝分隔线 / 纸面（≥1.10）"],
    ["color-border-secondary", "color-bg-card", 1.1, "发丝分隔线 / 卡片（≥1.10）"],
    // accent 的 soft 档（纸面淡染色）。2026-10-21 补测：暗色主题此前
    // 完全没覆盖这两档，继承 tokens.css 的「accent 混白」—— 暗底上翻成
    // 近白亮块，preview 的行内代码 / 引用块变成白底浅字（读不出来），
    // 外观页选中的选择卡变成白卡。dark.css 已改为「accent 混纸面底」，
    // 这几条配对就是防它再退化。
    ["color-accent", "color-accent-soft", 4.5, "accent 文字 / accent-soft 底（Badge brand、Button tonal、选中卡）"],
    ["color-accent", "color-accent-soft-strong", 4.5, "accent 文字 / accent-soft-strong 底（子代理状态、记忆标签）"],
    ["color-text-primary", "color-accent-soft", 4.5, "正文 / accent-soft 底（preview 行内代码、引用块）"],
    ["color-text-secondary", "color-accent-soft", 4.5, "次级 / accent-soft 底（preview 引用块）"],
    ["color-text-primary", "color-accent-soft-strong", 4.5, "正文 / accent-soft-strong 底（preview 目录当前项）"],
  ];

  const contrastFailures = [];
  const contrastSkipped = [];
  for (const themeId of ["light", "dark", "ink-green"]) {
    const vars = themeValues(themeId);
    if (!vars) continue;
    for (const [fgKey, bgKey, min, label] of CONTRAST_PAIRS) {
      const fgRaw = vars[fgKey];
      const bgRaw = vars[bgKey];
      if (!fgRaw || !bgRaw) continue;
      const fg = parseColor(fgRaw, vars);
      // 背景可能是半透明 → 合成到本主题纸面上
      const bg = resolveBg(bgRaw, vars);
      if (!fg || !bg) {
        // 解析不了就**报出来**，不能静默跳过 —— 静默跳过等于没测
        contrastSkipped.push({ themeId, label, fgKey, bgKey, fgRaw, bgRaw });
        continue;
      }
      const r = ratio(fg, bg);
      if (r !== null && r < min) {
        contrastFailures.push({ themeId, label, fgKey, bgKey, fgRaw, r, min });
      }
    }
  }
  if (contrastFailures.length > 0) {
    failures++;
    console.error(`[contrast] ${contrastFailures.length} 处配对未达 WCAG AA:`);
    for (const f of contrastFailures) {
      console.error(
        `  ${f.themeId}: ${f.label} — ${f.fgKey}(${f.fgRaw}) × ${f.bgKey} = ` +
          `${f.r.toFixed(2)}:1（需 ≥${f.min}）`,
      );
    }
    console.error(
      "  → 语义色的「文字安全档」用 --color-text-<hue>（保持色相、只压暗，\n" +
        "    两处都 ≥4.5:1）；控件描边见 --color-border-secondary 的取值。",
    );
  } else {
    console.log("[contrast] 全部配对达 WCAG AA ✓");
  }
  if (contrastSkipped.length > 0) {
    console.warn(
      `[contrast] WARN ${contrastSkipped.length} 处配对无法解析（未被测量）：`,
    );
    for (const s of contrastSkipped) {
      console.warn(
        `  ${s.themeId}: ${s.label} — ${s.fgKey}(${s.fgRaw}) × ${s.bgKey}(${s.bgRaw})`,
      );
    }
  }
}

process.exit(failures > 0 ? 1 : 0);
