// 共享预览层一致性门禁（CI 硬门禁）。
//
// 守住 2026-10-19 那次修复确立的三条约束（见
// docs/notes/implemented/architecture/2026-10-19-shared-preview-layer-ownership.md）：
//
//   1. 共享包 @hermix/preview 的组件类名必须能在共享 CSS
//      （packages/preview/src/styles/preview-doc.css）里找到规则。
//      —— 渲染器样式住在桌面端的 CSS 里时，viewer 侧的这些类名全是空操作，
//         而且类名不报错，样式静默丢失，排查成本极高。
//   2. 共享包不得使用 Tailwind 原子类。viewer 刻意不装 Tailwind
//      （apps/viewer/vite.config.ts 注释：最小化，无 Tailwind），
//      原子类在那边不生效。本条由第 1 条蕴含，但单独报出来更易读。
//   3. 共享 CSS 用到的每个 var(--token)，desktop 与 viewer 两侧都必须定义。
//      —— var() 解析失败会让**整条声明被丢弃**，圆角/间距/字号/行高/字重
//         同时静默失效，表现为"排版塌了但看不出原因"。
//
// 退出码：0 = 干净；1 = 有违规。

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const ROOT = new URL("..", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
const PKG = join(ROOT, "packages/preview/src");
const SHARED_CSS = join(PKG, "styles/preview-doc.css");

/** 应用层提供的类名：每个 app 各自定义（桌面端是 Tailwind @utility，
 *  viewer 是自己的 styles.css），不归共享 CSS 管。 */
const APP_LEVEL = new Set(["icon-sm", "icon-md", "icon-lg"]);

/** 语法高亮标记：`language-${lang}` 由 refractor 消费，不是样式类。 */
const isMarker = (c) => c.endsWith("-") || c === "language";

function walk(dir, out = []) {
  for (const f of readdirSync(dir)) {
    const p = join(dir, f);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (/\.tsx$/.test(f)) out.push(p);
  }
  return out;
}

const errors = [];
const fail = (m) => errors.push(m);

// ── 1) 共享组件的类名必须在共享 CSS 里有规则 ────────────────────
const sharedCss = readFileSync(SHARED_CSS, "utf8");
const defined = new Set();
for (const m of sharedCss.matchAll(/(^|[\s,>+~{])\.([a-z][a-z0-9-]*)(?![a-z0-9-])/gm)) {
  defined.add(m[2]);
}

const used = new Map(); // class -> [文件]
for (const f of walk(PKG)) {
  const src = readFileSync(f, "utf8");
  const seen = new Set();
  for (const re of [/className=\{?["'`]([^"'`]+)["'`]/g, /className=\{`([^`]+)`\}/g]) {
    for (const m of src.matchAll(re)) {
      for (const c of m[1].split(/[\s${}]+/)) {
        if (!c || !/^[a-z][a-z0-9-]*$/.test(c)) continue;
        if (seen.has(c)) continue;
        seen.add(c);
        if (!used.has(c)) used.set(c, []);
        used.get(c).push(relative(ROOT, f).replace(/\\/g, "/"));
      }
    }
  }
}

for (const [c, files] of [...used].sort()) {
  if (isMarker(c) || APP_LEVEL.has(c)) continue;
  if (defined.has(c)) continue;
  fail(
    `类名 "${c}"（${files.join(", ")}）在共享 CSS 里没有规则。\n` +
      `    共享包被 desktop 与 viewer 同时消费，样式必须住在 ` +
      `packages/preview/src/styles/preview-doc.css；\n` +
      `    若这是 Tailwind 原子类，viewer 侧不会生效（viewer 不装 Tailwind）。`,
  );
}

// ── 2) 共享 CSS 用到的 token，两侧都必须定义 ────────────────────
const tokens = new Set();
for (const m of sharedCss.matchAll(/var\(\s*(--[a-z0-9-]+)/g)) tokens.add(m[1]);

function definedTokens(files) {
  const def = new Set();
  for (const f of files) {
    let t;
    try { t = readFileSync(f, "utf8"); } catch { continue; }
    for (const m of t.matchAll(/(--[a-z0-9-]+)\s*:/g)) def.add(m[1]);
  }
  return def;
}

const viewerTokens = definedTokens([join(ROOT, "apps/viewer/src/styles.css")]);
const desktopTokens = definedTokens([
  join(ROOT, "apps/desktop-tauri/src/styles/tokens.css"),
  join(ROOT, "apps/desktop-tauri/src/styles/base.css"),
  join(ROOT, "apps/desktop-tauri/src/styles/globals.css"),
  join(ROOT, "apps/desktop-tauri/src/styles/code-view.css"),
  join(ROOT, "apps/desktop-tauri/src/styles/components-tool-card.css"),
  join(ROOT, "apps/desktop-tauri/src/styles/components-prose.css"),
  join(ROOT, "apps/desktop-tauri/src/styles/components-layout.css"),
  ...["light", "dark", "ink-green"]
    .map((t) => join(ROOT, `apps/desktop-tauri/src/styles/themes/${t}.css`)),
]);

for (const [label, have] of [["viewer", viewerTokens], ["desktop", desktopTokens]]) {
  const missing = [...tokens].filter((t) => !have.has(t)).sort();
  if (missing.length) {
    fail(
      `${label} 侧缺少共享 CSS 用到的 token：${missing.join(", ")}\n` +
        `    var() 解析失败会让整条声明被丢弃，样式静默失效。`,
    );
  }
}

if (errors.length) {
  console.log(`preview-layer: ${errors.length} 处违规`);
  for (const e of errors) console.log("  " + e);
  process.exit(1);
}
console.log(
  `preview-layer: OK（${used.size} 个类名全部有规则；${tokens.size} 个 token 两侧齐备）`,
);
