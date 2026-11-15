/**
 * desktop 样式层叠死块分析器（保守判定）。
 *
 * 目的：找出"完全同选择器、按级联顺序较早出现、且其所有声明属性都被
 * 后续同选择器规则覆盖"的规则块。这类块在 computed style 上零贡献，
 * 删除后视觉不变 —— 即上一轮 AUDIT 里说的"100% 死重复块"。
 *
 * 保守策略（宁可漏报不误删）：
 *   - 覆盖前提：覆盖规则与候选块的"归一化选择器"完全相同（含伪类/伪元素，
 *     即 :hover / ::before 这类块只在同选择器后续出现时才算覆盖）。
 *   - 媒体上下文：只在相同 @media 条件（或两者都无媒体）之间比较。
 *   - 简写展开：padding / margin / border 系列 / background / font / inset /
 *     border-radius 等简写展开为对应长写属性再逐属性比较，避免把
 *     "简写 + 单向长写"误判为全死。
 *   - 跳过 @keyframes 内部（非级联规则）、@font-face。
 *
 * 输出：候选死块清单，每条含 文件:行号、选择器、被覆盖证据
 *       （每个属性 -> 哪个文件的哪条规则覆盖）。
 *
 * 跑法：node apps/desktop-tauri/scripts/analyze-css-cascade.cjs
 */

const fs = require("fs");
const path = require("path");
const postcss = require("postcss");

const ROOT = path.resolve(__dirname, "..", "src", "styles");

// 加载顺序（来自 main.tsx 静态/动态 import + globals.css @import 链）。
// 见 scripts/... 注释；顺序即级联优先级，前者被后者覆盖。
const ORDER = [
  "screenshot-overlay.css",
  "tokens.css",
  "base.css",
  "components-layout.css",
  "components-controls.css",
  "components-prose.css",
  "components-tool-card.css",
  "process-line.css",
  "components-helpers.css",
  "inline-thinking-dot.css",
  "code-view.css",
  "utilities.css",
  "workbench.css",
  "inline-approval-card.css",
  // 主题层最后（data-theme 切换，含属性选择器，特异性更高）
  path.join("themes", "light.css"),
  path.join("themes", "dark.css"),
];

// 简写 -> 展开的长写属性（仅本项目实际用到的）。
const SHORTHAND_EXPAND = {
  padding: ["padding-top", "padding-right", "padding-bottom", "padding-left"],
  margin: ["margin-top", "margin-right", "margin-bottom", "margin-left"],
  "border-radius": [
    "border-top-left-radius",
    "border-top-right-radius",
    "border-bottom-right-radius",
    "border-bottom-left-radius",
  ],
  inset: ["top", "right", "bottom", "left"],
  background: [
    "background-color",
    "background-image",
    "background-repeat",
    "background-position",
    "background-size",
    "background-attachment",
  ],
  font: [
    "font-style",
    "font-variant",
    "font-weight",
    "font-size",
    "line-height",
    "font-family",
  ],
  overflow: ["overflow-x", "overflow-y"],
  border: ["border-width", "border-style", "border-color"],
  "border-top": ["border-top-width", "border-top-style", "border-top-color"],
  "border-right": ["border-right-width", "border-right-style", "border-right-color"],
  "border-bottom": ["border-bottom-width", "border-bottom-style", "border-bottom-color"],
  "border-left": ["border-left-width", "border-left-style", "border-left-color"],
  "border-width": [
    "border-top-width",
    "border-right-width",
    "border-bottom-width",
    "border-left-width",
  ],
  "border-style": [
    "border-top-style",
    "border-right-style",
    "border-bottom-style",
    "border-left-style",
  ],
  "border-color": [
    "border-top-color",
    "border-right-color",
    "border-bottom-color",
    "border-left-color",
  ],
  flex: ["flex-grow", "flex-shrink", "flex-basis"],
  "grid-area": ["grid-row-start", "grid-column-start", "grid-row-end", "grid-column-end"],
  gap: ["row-gap", "column-gap"],
  transition: [], // transition 不展开（组合属性，比较困难，跳过）
};

function expandProp(prop) {
  if (SHORTHAND_EXPAND[prop]) return SHORTHAND_EXPAND[prop];
  return [prop];
}

// 把一条规则的声明集合展开为 { 长写属性: [{value, important, line}] }
function expandDecls(decls) {
  const map = new Map();
  for (const d of decls) {
    const props = expandProp(d.prop);
    for (const p of props) {
      if (!map.has(p)) map.set(p, []);
      map.get(p).push({ value: d.value, important: !!d.important, line: d.line });
    }
  }
  return map;
}

function normalizeSelector(sel) {
  return sel
    .replace(/\s+/g, " ")
    .replace(/\s*([>+~])\s*/g, "$1")
    .replace(/\s*,\s*/g, ",")
    .trim();
}

// 收集全部规则（含源文件、行号、选择器、声明、媒体上下文、特异性）。
const rules = [];
for (const rel of ORDER) {
  const file = path.join(ROOT, rel);
  if (!fs.existsSync(file)) {
    console.error("SKIP missing:", rel);
    continue;
  }
  const css = fs.readFileSync(file, "utf8");
  const root = postcss.parse(css, { from: file });
  const fileTag = rel;
  root.walkRules((rule) => {
    // 跳过 keyframes 内部
    let p = rule.parent;
    while (p && p.type === "rule") p = p.parent; // 跨嵌套规则
    if (p && p.type === "atrule" && p.name === "keyframes") return;
    if (p && p.type === "atrule" && p.name === "font-face") return;
    // 媒体上下文
    let media = "";
    let q = rule.parent;
    while (q) {
      if (q.type === "atrule" && (q.name === "media" || q.name === "supports")) {
        media = (media ? media + " | " : "") + q.name + " " + (q.params || "");
      }
      q = q.parent;
    }
    const decls = rule.nodes
      .filter((n) => n.type === "decl")
      .map((n) => ({
        prop: n.prop,
        value: n.value,
        important: !!n.important,
        line: n.source && n.source.start ? n.source.start.line : 0,
      }));
    const selectors = rule.selector
      .split(",")
      .map((s) => normalizeSelector(s))
      .filter(Boolean);
    for (const sel of selectors) {
      rules.push({
        file: fileTag,
        line: rule.source && rule.source.start ? rule.source.start.line : 0,
        sel,
        rawSel: rule.selector,
        decls,
        expanded: expandDecls(decls),
        media,
      });
    }
  });
}

// 判定：对于候选块，是否所有展开属性都被后续同选择器、同媒体、特异性相等的
// 规则"最终"（取最后一个声明该属性的后续规则）覆盖。
// 处理 !important：若候选某属性是 important 且最终覆盖者非 important，则候选
// 仍生效（important 赢），该块不算死块（保守，宁可漏删）。
function lastCover(r, prop, later) {
  let best = null;
  for (const x of later) {
    if (x.expanded.has(prop)) {
      const cov = x.expanded.get(prop)[0];
      best = { file: x.file, line: x.line, value: cov.value, important: cov.important };
    }
  }
  return best;
}

const dead = [];
for (let i = 0; i < rules.length; i++) {
  const r = rules[i];
  if (r.decls.length === 0) continue; // 无声明
  const later = rules
    .slice(i + 1)
    .filter((x) => x.sel === r.sel && x.media === r.media);
  if (later.length === 0) continue;
  const evidence = [];
  let allCovered = true;
  for (const [prop, items] of r.expanded) {
    const candImportant = items.some((it) => it.important);
    const cov = lastCover(r, prop, later);
    if (!cov) {
      allCovered = false;
      break;
    }
    // important 反向：候选 important 且覆盖者非 important -> 候选仍生效
    if (candImportant && !cov.important) {
      allCovered = false;
      break;
    }
    evidence.push({ prop, by: cov });
  }
  if (allCovered) {
    dead.push({
      file: r.file,
      line: r.line,
      sel: r.sel,
      rawSel: r.rawSel,
      declCount: r.decls.length,
      evidence,
    });
  }
}

// 输出
console.log("=== 候选死重复块（按级联顺序较早、所有属性被后续同选择器覆盖） ===");
console.log("共 " + dead.length + " 处\n");
for (const d of dead) {
  console.log(`[${d.file}:${d.line}] ${d.rawSel}  (${d.declCount} 条声明)`);
  for (const e of d.evidence) {
    console.log(`    ${e.prop}  <-  ${e.by.file}:${e.by.line} (${e.by.value})`);
  }
  console.log("");
}
