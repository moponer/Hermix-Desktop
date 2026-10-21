#!/usr/bin/env node
/**
 * 守卫：组件里 `var(--x)` 引用的自定义属性必须真有定义。
 *
 * 为什么需要它（2026-10-11 事故）：
 * VaultPanel.tsx 写了 `bg-[var(--accent)]` + `text-white`。本项目的颜色令牌
 * 全部带 `--color-` 前缀，`--accent` 从来不存在 —— Tailwind 会照常发出一条
 * `background-color: var(--accent)`，该 var 未定义 → 声明在**计算值阶段**失效
 * → 底色回落 transparent，而 `text-white` 照常生效 → 实色按钮变成白底白字，
 * 肉眼完全看不见；`bg-[var(--bg-primary)]` 让弹窗白底消失、页面内容从弹窗里
 * 透出来。全程 0 报错、0 构建失败、现有守卫全绿。
 *
 * 为什么会漏掉：`check-theme-tokens.mjs` 查的是「主题文件是否覆盖齐 CONTRACT
 * 令牌」，只管**定义侧**；没有东西检查**引用侧**。这类"打错前缀 → 静默隐形"
 * 的 bug 因此在两道守卫之间穿过去了。本脚本补的就是引用侧。
 *
 * 定义来源（全部扫描，任一命中即算已定义）：
 *   1. apps/desktop-tauri 下所有 *.css 的 `--name: value`
 *   2. index.html 内联 <style> 里的 `--name: value`
 *   3. TS/TSX 里 `setProperty("--name", …)` 的运行时注入（useTheme / useWallpaper）
 *
 * 已知外部命名空间（第三方库 / Tailwind 内部）走 ALLOW_PREFIXES 白名单。
 *
 * 用法：node scripts/check-css-var-refs.cjs [--root <apps/desktop-tauri>]
 * 退出码：0 = 全部有定义；1 = 存在未定义引用。
 */

const fs = require("fs");
const path = require("path");

const args = process.argv.slice(2);
const rootArgIdx = args.indexOf("--root");
const APP_ROOT = path.resolve(
  rootArgIdx >= 0 && args[rootArgIdx + 1] ? args[rootArgIdx + 1] : path.join(__dirname, ".."),
);

/** 第三方 / 运行时命名空间白名单（不属于本项目设计令牌）。 */
const ALLOW_PREFIXES = [
  "--tw-", // Tailwind 内部（ring / shadow / translate …）
  "--radix-", // Radix UI
  "--vite-", // Vite HMR
  "--react-",
  "--scrollbar-", // 浏览器非标扩展
  "--safe-area", // iOS 安全区
  "-webkit-",
];

const SKIP_DIRS = new Set(["node_modules", "dist", "target", ".git", "release", "src-tauri"]);

function walk(dir, out) {
  let entries;
  try {
    entries = fs.readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const e of entries) {
    if (e.isDirectory()) {
      if (SKIP_DIRS.has(e.name)) continue;
      walk(path.join(dir, e.name), out);
    } else {
      out.push(path.join(dir, e.name));
    }
  }
  return out;
}

/** 剥掉注释，避免「注释里提到的 var(--x)」被当成真引用、或注释里的
 *  `--x: y` 被当成真定义（本项目 tokens.css 注释里大量出现 var() 示例）。 */
function stripComments(src, isCss) {
  let s = src.replace(/\/\*[\s\S]*?\*\//g, (m) => m.replace(/[^\n]/g, " "));
  if (!isCss) {
    // 行注释：跳过 "://" 这类协议头，避免把 URL 一起删掉。
    s = s.replace(/(^|[^:])\/\/[^\n]*/g, (m, p1) => p1 + " ".repeat(m.length - p1.length));
  }
  return s;
}

const files = walk(APP_ROOT, []);
const cssFiles = files.filter((f) => f.endsWith(".css"));
const codeFiles = files.filter((f) => /\.(ts|tsx)$/.test(f));
const htmlFiles = files.filter((f) => f.endsWith(".html"));

// ── 收集定义 ────────────────────────────────────────────────────────────
const defined = new Map(); // name -> "file:line"（首个定义处）

function noteDefinition(name, file, line) {
  if (!defined.has(name)) defined.set(name, `${path.relative(APP_ROOT, file)}:${line}`);
}

function collectFromText(text, file, isCss) {
  const clean = stripComments(text, isCss);
  // CSS 定义：`--name:`（允许前面有空白/属性上下文）
  // 2026-10-21：前缀里补上引号 —— HTML 的内联 style 形如
  // `style="--tc:var(--t1)"`，此前因为前面是 `"` 而没被认成定义，
  // 引用侧 `var(--tc)` 就会被误报成「未定义」。
  const reDef = /(?:^|[\s;{"'])(--[A-Za-z0-9_-]+)\s*:/g;
  let m;
  while ((m = reDef.exec(clean))) {
    const line = clean.slice(0, m.index).split("\n").length;
    noteDefinition(m[1], file, line);
  }
}

for (const f of cssFiles) collectFromText(fs.readFileSync(f, "utf8"), f, true);
for (const f of htmlFiles) collectFromText(fs.readFileSync(f, "utf8"), f, true);

// TS/TSX 里的运行时定义，两种形态：
//   1. setProperty("--name", …)
//   2. React 内联样式对象键：{ "--ab-left-w": `${w}px` }（App.tsx 的可拖列宽）
// 2026-10-21：HTML 里的 <script> 同样会用 setProperty 注入主题变量
// （design-previews 的 `win.style.setProperty("--tc", t.color)`），
// 原先只扫 TS/TSX，HTML 内联脚本里的运行时定义被漏掉 → 误报。
for (const f of [...codeFiles, ...htmlFiles]) {
  const clean = stripComments(fs.readFileSync(f, "utf8"), false);
  const patterns = [/setProperty\(\s*["'](--[A-Za-z0-9_-]+)["']/g, /["'](--[A-Za-z0-9_-]+)["']\s*:/g];
  for (const re of patterns) {
    let m;
    while ((m = re.exec(clean))) {
      const line = clean.slice(0, m.index).split("\n").length;
      noteDefinition(m[1], f, line);
    }
  }
}

// ── 收集引用 ────────────────────────────────────────────────────────────
const refs = [];

/** 找到 body 里第一个「顶层」逗号（跳过嵌套括号内��逗号）。 */
function topLevelComma(body) {
  let depth = 0;
  for (let i = 0; i < body.length; i++) {
    const ch = body[i];
    if (ch === "(") depth++;
    else if (ch === ")") depth--;
    else if (ch === "," && depth === 0) return i;
  }
  return -1;
}

/**
 * 解析一个 var() 调用的内容，返回 { name, hasFallback, fallback }。
 *
 * 2026-10-21：此前只用正则抓 `var(--name`，**不看有没有 fallback**，
 * 于是 `var(--tb-left, 280px)` 这类完全合法的写法被判成「未定义引用」——
 * 仓库里 19 处全是这种误报（design-previews 的存档 HTML），硬门禁直接变红。
 *
 * CSS 规范里 `var(--x, fallback)` 在 --x 未定义时用 fallback，**声明不会失效**，
 * 只有「未定义且无 fallback」才会让整条声明在计算值阶段掉链子 —— 那才是
 * 本脚本要抓的 bug。带 fallback 的引用改为**递归检查 fallback 内部**：
 * `var(--a, var(--b))` 若 --b 也未定义且无 fallback，声明同样会失效。
 */
function parseVarCall(clean, varStart) {
  const open = clean.indexOf("(", varStart);
  if (open < 0) return null;
  let depth = 1;
  let i = open + 1;
  while (i < clean.length && depth > 0) {
    const ch = clean[i];
    if (ch === "(") depth++;
    else if (ch === ")") depth--;
    i++;
  }
  const body = clean.slice(open + 1, i - 1);
  const comma = topLevelComma(body);
  if (comma < 0) return { name: body.trim(), hasFallback: false, fallback: "" };
  const name = body.slice(0, comma).trim();
  const fallback = body.slice(comma + 1).trim();
  return { name, hasFallback: fallback.length > 0, fallback };
}

function collectRefs(text, file, isCss) {
  const clean = stripComments(text, isCss);
  const reRef = /var\(\s*(--[A-Za-z0-9_-]+)/g;
  let m;
  while ((m = reRef.exec(clean))) {
    const line = clean.slice(0, m.index).split("\n").length;
    // 只取该行，便于报告
    const lineText = clean.split("\n")[line - 1] || "";
    const call = parseVarCall(clean, m.index);
    if (!call) continue;
    refs.push({
      name: call.name,
      hasFallback: call.hasFallback,
      fallback: call.fallback,
      file: path.relative(APP_ROOT, file),
      line,
      lineText: lineText.trim(),
    });
  }
}

for (const f of cssFiles) collectRefs(fs.readFileSync(f, "utf8"), f, true);
for (const f of codeFiles) collectRefs(fs.readFileSync(f, "utf8"), f, false);
for (const f of htmlFiles) collectRefs(fs.readFileSync(f, "utf8"), f, true);

// ── 判定 ────────────────────────────────────────────────────────────────
const allowed = (name) => ALLOW_PREFIXES.some((p) => name.startsWith(p));

/**
 * 这条引用会不会让声明在计算值阶段失效？
 *   未定义 + 无 fallback  → 失效（真 bug，本脚本要抓的就是这个）
 *   未定义 + 有 fallback  → 声明不失效（合法写法），但要**递归看 fallback 内部**：
 *                          `var(--a, var(--b))` 若 --b 未定义且无 fallback，整条仍然失效
 */
function isBroken(r, depth = 0) {
  if (depth > 4) return false;
  if (!r || !r.name) return false;
  if (allowed(r.name) || defined.has(r.name)) return false;
  if (!r.hasFallback) return true;
  const inner = /var\(\s*(--[A-Za-z0-9_-]+)/g;
  let m;
  while ((m = inner.exec(r.fallback || ""))) {
    const call = parseVarCall(r.fallback, m.index);
    if (call && isBroken(call, depth + 1)) return true;
  }
  return false;
}

const missing = refs.filter((r) => isBroken(r));

const uniq = new Map();
for (const r of missing) {
  const key = r.file + ":" + r.line;
  if (!uniq.has(key)) uniq.set(key, r);
}

console.log("============================================");
console.log("  CSS 变量引用守卫（var(--x) 必须有定义）");
console.log("============================================");
console.log(`扫描：${cssFiles.length} 个 CSS · ${codeFiles.length} 个 TS/TSX · ${htmlFiles.length} 个 HTML`);
console.log(`已定义自定义属性：${defined.size} 个 · 引用点：${refs.length} 处`);

if (uniq.size === 0) {
  console.log("");
  console.log("PASS — 所有 var(--x) 引用都能在令牌文件 / 内联样式 / setProperty 中找到定义。");
  process.exit(0);
}

console.log("");
console.log(`FAIL — 发现 ${uniq.size} 处引用未定义的自定义属性：`);
console.log("");
for (const r of uniq.values()) {
  console.log(`  ${r.file}:${r.line}  →  ${r.name}`);
  console.log(`      ${r.lineText.slice(0, 120)}`);
}
console.log("");
console.log("说明：未定义的 var() 会让整条声明在计算值阶段失效。若同时写了");
console.log("`text-white`，实色按钮就会变成白底白字（隐形）而不报任何错。");
console.log("修法：补 `--color-` 前缀，或改用 tokens.css 暴露的工具类（bg-accent …）。");
process.exit(1);
