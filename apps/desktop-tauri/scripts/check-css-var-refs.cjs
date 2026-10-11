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
  const reDef = /(?:^|[\s;{])(--[A-Za-z0-9_-]+)\s*:/g;
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
for (const f of codeFiles) {
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
function collectRefs(text, file, isCss) {
  const clean = stripComments(text, isCss);
  const reRef = /var\(\s*(--[A-Za-z0-9_-]+)/g;
  let m;
  while ((m = reRef.exec(clean))) {
    const line = clean.slice(0, m.index).split("\n").length;
    // 只取该行，便于报告
    const lineText = clean.split("\n")[line - 1] || "";
    refs.push({ name: m[1], file: path.relative(APP_ROOT, file), line, lineText: lineText.trim() });
  }
}

for (const f of cssFiles) collectRefs(fs.readFileSync(f, "utf8"), f, true);
for (const f of codeFiles) collectRefs(fs.readFileSync(f, "utf8"), f, false);
for (const f of htmlFiles) collectRefs(fs.readFileSync(f, "utf8"), f, true);

// ── 判定 ────────────────────────────────────────────────────────────────
const allowed = (name) => ALLOW_PREFIXES.some((p) => name.startsWith(p));
const missing = refs.filter((r) => !defined.has(r.name) && !allowed(r.name));

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
