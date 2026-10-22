#!/usr/bin/env node
// 设计约束门禁（DesignPrimitives）—— check-design-guard.mjs 的补充。
//
// 为什么单独一个脚本：guard 负责「不许写什么」（字面色 / dark: / 加载中…），
// 本脚本负责「不许漏什么」——可访问名、描边反模式、图标档位、原语采用率。
// 这两类规则混在一个脚本里会让「新增规则即新增失败」的噪音变大。
//
// 路径解析：**从脚本自身位置推导仓库根**，不用 process.cwd()。
// check-design-guard.mjs 用 join(process.cwd(), "src")，在仓库根跑会 ENOENT 崩
// （ci-gate.sh 因此必须先 cd apps/desktop-tauri）。本脚本没有这个坑——
// 任何目录跑结果都一样。
//
// 退出码：
//   默认（advisory）：有发现也 exit 0，但逐条打印 + 汇总计数。用于「先看见，
//     再收紧」——新规则上线时仓库里已有一批存量违规，直接阻断会把 CI 打红。
//   --strict：有发现 exit 1。存量收敛完毕后，用它接替 advisory。
//
// 用法：
//   node scripts/check-design-primitives.mjs            # advisory
//   node scripts/check-design-primitives.mjs --strict   # 阻断
//   node scripts/check-design-primitives.mjs --rule=G3  # 只跑某条规则

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = join(HERE, "..");
const SRC = join(ROOT, "apps", "desktop-tauri", "src");

const STRICT = process.argv.includes("--strict");
const ONLY = process.argv
  .filter((a) => a.startsWith("--rule="))
  .map((a) => a.slice("--rule=".length));

const EXT = /\.(tsx|ts)$/;
const SKIP = /(\.test\.|\.d\.ts$)/;

/* ── 规则定义 ────────────────────────────────────────────────
   每条规则：id / 说明 / 检测函数。检测函数返回违规条目数组。 */

const findings = [];
const add = (group, file, line, text) =>
  findings.push({ group, file: relative(ROOT, file), line, text: String(text).trim().slice(0, 160) });

function* walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) yield* walk(p);
    else if (EXT.test(name) && !SKIP.test(p.replaceAll("\\", "/"))) yield p;
  }
}

function* walkCss(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) yield* walkCss(p);
    else if (name.endsWith(".css")) yield p;
  }
}

const files = [...walk(SRC)];
const cssFiles = [...walkCss(SRC)];

/* ── 工具：从 JSX 源码里抽出某个元素的开标签文本 ──────────────
   难点：属性里可能有 `>`（`onClick={() => x}`）和引号。
   解法：逐字符扫，跟踪引号状态与花括号深度。 */

function openTag(src, start) {
  // start 指向 '<' 的下一个字符
  let i = start;
  let depth = 0;
  let quote = null;
  while (i < src.length) {
    const c = src[i];
    if (quote) {
      if (c === "\\") { i += 2; continue; }
      if (c === quote) quote = null;
      i++;
      continue;
    }
    if (c === '"' || c === "'" || c === "`") { quote = c; i++; continue; }
    if (c === "{") { depth++; i++; continue; }
    if (c === "}") { depth--; i++; continue; }
    if (c === ">" && depth === 0) return { text: src.slice(start, i), end: i };
    i++;
  }
  return null;
}

/* ── G3：图标按钮必须有无障碍名 ─────────────────────────────
   范围限定为 className 明确是「图标按钮」的元素（icon-btn /
   titlebar-btn / icon-button / modal-close）——这类可从属性串可靠判定，
   覆盖的正是高风险的一批。
   （原 check-design-guard.mjs 的 G3 曾因全量扫 <button> 产生 90%+ 误报
   而被移除，改为限定范围后可以恢复。） */

const ICON_BTN_CLASS = /\b(icon-btn|titlebar-btn|icon-button|modal-close)\b/;

function checkG3() {
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    let idx = 0;
    while ((idx = src.indexOf("<button", idx)) !== -1) {
      const tag = openTag(src, idx + 1);
      if (!tag) break;
      const { text, end } = tag;
      idx = end;
      const cls = /className\s*=\s*(?:"([^"]*)"|\{`([^`]*)`\}|\{'([^']*)'\})/.exec(text);
      const clsText = cls ? cls[1] || cls[2] || cls[3] || "" : "";
      if (!ICON_BTN_CLASS.test(clsText)) continue;
      const hasName = /\baria-label(?:ledby)?\s*=|\btitle\s*=/.test(text);
      if (!hasName) {
        const line = src.slice(0, idx - text.length).split("\n").length;
        add("G3", file, line, text.replace(/\s+/g, " "));
      }
    }
  }
}

/* ── G6：描边反模式 ──────────────────────────────────────────
   G6a 虚线描边（空态虚线框，用户明确不要描边）
   G6b 单侧色条 side-tab（impeccable-design.md 的头号反模式）

   三条判准，避免误报：
   ① G6a 只看**真正落到元素上**的 className 属性，不看原语里「预留的变体」
      （ui/EmptyState 的 dashed 变体、ui/Card 的 dashed 变体）—— 变体是选项，
      不是既成事实，默认值已经改成 plain。
   ② G6b 尊重仓库自己的豁免约定（impeccable-design.md:45-46）：
      文件头有 `impeccable-disable side-tab: <理由>` 的，该文件整条跳过。
   ③ CSS 里的注释块整块跳过（注释里提到反模式是**记录**，不是违规）。 */

function checkG6() {
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    const lines = src.split("\n");
    lines.forEach((ln, i) => {
      if (/^\s*(\/\/|\*|\/\*)/.test(ln)) return;
      // ① 真正的 className 属性里出现虚线描边
      if (/className\s*=\s*["'`][^"'`]*\bborder-dashed\b/.test(ln)) {
        add("G6a", file, i + 1, ln);
      }
    });
  }
  for (const file of cssFiles) {
    const src = readFileSync(file, "utf8");
    // ② 豁免：文件头有 impeccable-disable side-tab 的整条跳过
    if (/impeccable-disable\s+side-tab/.test(src)) continue;
    const lines = src.split("\n");
    let inComment = false;
    lines.forEach((ln, i) => {
      // ③ 注释块内跳过
      if (inComment) {
        if (ln.includes("*/")) inComment = false;
        return;
      }
      if (ln.includes("/*") && !ln.includes("*/")) { inComment = true; return; }
      if (/^\s*(\/\/|\*)/.test(ln)) return;
      const m = /border-left\s*:\s*(\d+)px\s+solid/.exec(ln);
      if (m && Number(m[1]) >= 2) {
        add("G6b", file, i + 1, ln);
      }
      if (/\bborder-l-(4|2)\b|\bborder-l-\[(2|3|4)px\]/.test(ln)) {
        add("G6b", file, i + 1, ln);
      }
    });
  }
}

/* ── G7：图标尺寸档位 ────────────────────────────────────────
   tokens.css 的契约：icon 档位 = 10/12/14/16；
   特殊尺寸 = 22/24/28/32/36/48/72（品牌/Logo/空态大图标/灯箱）。 */

const ICON_SIZE_OK = new Set([10, 12, 14, 16, 22, 24, 28, 32, 36, 48, 72]);

function checkG7() {
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    const lines = src.split("\n");
    lines.forEach((ln, i) => {
      // size={N} 数字字面量
      for (const m of ln.matchAll(/\bsize=\{(\d{1,3})\}/g)) {
        const n = Number(m[1]);
        if (!ICON_SIZE_OK.has(n)) {
          add("G7", file, i + 1, m[0]);
        }
      }
      // 图标元素上的裸 Tailwind 尺寸（size-[Npx] / w-N h-N）
      // 只对**图标组件**判：className 里带 icon-* 的，或元素名是 lucide 图标。
      const el = /<([A-Z][A-Za-z0-9]*)\b([^>]*)>/.exec(ln);
      if (!el) return;
      const attrs = el[2];
      if (!/\bclassName\s*=/.test(attrs)) return;
      if (!/\bicon-(xs|sm|md|lg)\b/.test(attrs) && !/\bsize-\[|\bw-\d+\s+h-\d+/.test(attrs)) return;
      if (/\bsize-\[(\d{1,3})px\]/.test(attrs)) {
        add("G7", file, i + 1, `${el[1]} ... ${attrs.trim().slice(0, 90)}`);
      } else if (/\bw-(\d+)\s+h-(\d+)\b/.test(attrs) && !/\bw-\d+\s+h-\d+/.test(attrs.replace(/\bw-(\d+)\s+h-\d+\b/, ""))) {
        // 只有当整个 className 没有 icon-* 档位、却有裸 w-N h-N 时才算绕过
        add("G7", file, i + 1, `${el[1]} ... ${attrs.trim().slice(0, 90)}`);
      }
    });
  }
}

/* ── G8：表单控件高度档位 ────────────────────────────────────
   契约（宪法 §五）：控件高度 24/30/32/40/44 五档。
   Tailwind 名 → px：h-6=24 h-7=28 h-8=32 h-9=36 h-10=40 h-11=44。
   只对真正的 <input>/<select>/<textarea> 元素判：
   - 元素名必须出现在标签开头（`select-none` 这类 class 不算元素）；
   - 排除 min-h-/max-h- 前缀（min-h-40 是 textarea 的最小高度，合法）。 */

const CTRL_H_OK = new Set([6, 8, 10, 11]); // h-6/8/10/11 → 24/32/40/44

function checkG8() {
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    const lines = src.split("\n");
    lines.forEach((ln, i) => {
      // 元素名必须紧跟在 < 后面，且开标签在同一行
      const el = /<(input|select|textarea)\b/.exec(ln);
      if (!el) return;
      if (/\btype\s*=\s*"(checkbox|radio|hidden)"/.test(ln)) return;
      if (!/className\s*=/.test(ln)) return;
      // 去掉 min-h-/max-h- 再找 h-N
      const cleaned = ln.replace(/\b(?:min|max)-h-\d+/g, "");
      for (const m of cleaned.matchAll(/(?<![\w-])h-(\d+)\b/g)) {
        const n = Number(m[1]);
        if (!CTRL_H_OK.has(n)) {
          add("G8", file, i + 1, m[0]);
        }
      }
    });
  }
}

/* ── G9：原始语义色当文字色用 ────────────────────────────────
   `--color-success` / `--color-warning` / `--color-error` / `--color-info`
   是**图标 / 描边 / 填充**用的身份色；12px 以下的文字必须用
   `--color-text-*` 文字安全档（两处都 ≥4.5:1）。
   判据：类名落在**非图标**元素上。图标元素（Lucide 组件）用原始色没问题。 */

/* 注意 lookbehind：`text-text-error`（已迁移到文字安全档的形态）里也含
   `text-error` 子串；`(?<![\w-])` 保证前面是 `-` 时**不**匹配，
   否则迁移完的文件反而会被这条规则继续报出来。
   `hover:text-error` 前面是 `:`，仍然匹配 —— 那是 hover 态的原始色，也要迁移。 */
const RAW_SEMANTIC_TEXT = /(?<![\w-])text-(warning|success|error|info)(?![\w-])/;

function checkG9() {
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    let idx = 0;
    while ((idx = src.indexOf("<", idx)) !== -1) {
      const tag = openTag(src, idx + 1);
      if (!tag) break;
      const { text, end } = tag;
      const next = src.indexOf("<", end);
      idx = end;
      if (next !== -1 && next < end + 400 && /^\s*$/.test(src.slice(end, next))) continue;
      // 元素名：<div ...> / <AlertTriangle ...>
      const nameM = /^([A-Za-z][A-Za-z0-9.]*)/.exec(text);
      if (!nameM) continue;
      const elName = nameM[1];
      if (/^[A-Z]/.test(elName)) continue; // React 组件（Lucide 图标）→ 原始色可用
      const cls = /className\s*=\s*"([^"]*)"/.exec(text) || /className\s*=\s*\{`([^`]*)`\}/.exec(text);
      if (!cls) continue;
      if (!RAW_SEMANTIC_TEXT.test(cls[1])) continue;
      const line = src.slice(0, idx - text.length).split("\n").length;
      add("G9", file, line, `${elName} ... ${cls[1].trim().slice(0, 90)}`);
    }
  }
  // G9 的第二面：原始语义色出现在**字符串值**里（tone 映射、style 对象的
  // color 属性），而不是 className 属性 —— 2026-10-21 第一版迁移漏了这一类。
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    src.split("\n").forEach((ln, i) => {
      if (/^\s*(\/\/|\*)/.test(ln)) return;
      // tone: "text-error" / cls: "text-error bg-error-soft"
      if (/"text-(warning|success|error|info)\b/.test(ln)) {
        add("G9", file, i + 1, ln.trim().slice(0, 110));
      }
      // color: "var(--color-error)" —— 文字色直接用了原始语义色
      if (/\b(?:color|text)\s*:\s*"var\(--color-(warning|success|error|info)\)"/.test(ln)) {
        add("G9", file, i + 1, ln.trim().slice(0, 110));
      }
    });
  }
}

/* ── 原语采用率：零 import 的 ui/ 原语 ───────────────────────
   ui/index.ts 导出的名字，若在整个 src 下没有被从 ui 目录 import 过，
   说明这个原语是「死原语」——假装体系里有统一做法，实际没人用。 */

function checkPrimitives() {
  const idxPath = join(SRC, "components", "ui", "index.ts");
  const idx = readFileSync(idxPath, "utf8");
  const exported = new Set();
  for (const m of idx.matchAll(/export\s*\{([^}]+)\}/g)) {
    for (const part of m[1].split(",")) {
      const name = part.trim().split(/\s+as\s+/).pop()?.trim();
      if (name && /^[A-Z]/.test(name)) exported.add(name);
    }
  }
  const UI_DIR = join(SRC, "components", "ui");
  for (const name of exported) {
    let used = false;
    for (const file of files) {
      if (file === idxPath) continue;
      // 原语自己的定义文件不算「使用」——它是自己。
      const ownFile = join(UI_DIR, `${name}.tsx`);
      if (file === ownFile) continue;
      const src = readFileSync(file, "utf8");
      if (file.startsWith(UI_DIR)) {
        // ui/ 内部的组合（SkeletonCard 用 SkeletonText）算「使用」——
        // 原语之间互相组合是正当用法。
        if (new RegExp(`\\b${name}\\b`).test(src)) { used = true; break; }
        continue;
      }
      if (new RegExp(`from\\s+["'][^"']*components/ui["']`).test(src) ||
          new RegExp(`from\\s+["'][^"']*/ui["']`).test(src)) {
        if (new RegExp(`\\b${name}\\b`).test(src)) { used = true; break; }
      }
    }
    if (!used) {
      add("P0", idxPath, 1, name);
    }
  }
}

/* ── G10：中性结构描边不得超过基线（棘轮）────────────────────
   用户明确「UI 一律不用描边，层次靠底色差 + 阴影 + 间距」。
   2026-10-21 清掉了 238 处里的 211 处；2026-10-23 扁平化收口再清到 15 处
   （Composer / 自动化页 / 专家面板 / 更新面板的分隔线改为间距或淡染底；
    阿黎页、代码块、引用块在 CSS 层整片去线 —— 那部分不在本规则的统计里）。
   剩下 15 处是刻意保留的：
     · 复选框自身的方框（去掉就看不见控件了）
     · spinner 圆环（描边就是图形）
     · 状态横幅的语义描边（ACUI 健康卡 / 压缩横幅）
     · 图片边框
     · 少数浮层内部分隔
   所以不能一刀切禁掉，而是**棘轮**：数量不得增加。
   每次清理后把 BASELINE 调低，新代码再画中性描边就会让门禁变红。 */
const STRUCTURAL_BORDER_BASELINE = 15;

const NEUTRAL_BORDER = new RegExp(
  "^(border|border-border|border-border-primary|border-border-secondary|border-border-subtle|" +
    "border-primary|border-secondary|border-subtle|" +
    "border-\\[var\\(--color-border-(primary|secondary|subtle)\\)\\]|" +
    "border-\\[color:var\\(--color-border-(primary|secondary|subtle)\\)\\])$",
);
const CLASS_TOKEN = /(?:^|\s)((?:[a-z0-9-]+:)?[a-z0-9-]+(?:\/[0-9]+)?)/g;

function checkG10() {
  let count = 0;
  const places = [];
  for (const file of files) {
    const src = readFileSync(file, "utf8");
    for (const m of src.matchAll(/className=(?:"([^"]*)"|\{`([^`]*)`\})/g)) {
      const cls = m[1] || m[2] || "";
      for (const t of [...cls.matchAll(CLASS_TOKEN)].map((x) => x[1])) {
        if (!NEUTRAL_BORDER.test(t)) continue;
        count++;
        const line = src.slice(0, m.index).split("\n").length;
        places.push(`${relative(SRC, file)}:${line}  ${t}`);
      }
    }
  }
  if (count > STRUCTURAL_BORDER_BASELINE) {
    add("G10", "(全仓)", 0, `中性结构描边 ${count} 处 > 基线 ${STRUCTURAL_BORDER_BASELINE} 处（新增 ${count - STRUCTURAL_BORDER_BASELINE} 处）`);
    for (const p of places.slice(0, 20)) add("G10", "(全仓)", 0, p);
  }
}

const RULES = [
  ["G3", "图标按钮缺无障碍名", checkG3],
  ["G6a", "虚线描边（用户明确不要描边）", checkG6],
  ["G6b", "单侧色条 side-tab（impeccable 头号反模式）", checkG6],
  ["G7", "图标尺寸偏离档位", checkG7],
  ["G8", "表单控件高度偏离档位", checkG8],
  ["G9", "原始语义色当文字色用（应走 --color-text-*）", checkG9],
  ["G10", "中性结构描边超过基线", checkG10],
  ["P0", "ui/ 原语零调用", checkPrimitives],
];

const active = RULES.filter(([id]) => !ONLY.length || ONLY.includes(id));
// 每条规则函数只跑一次（checkG6 同时服务 G6a / G6b，不能跑两遍——
// 否则同一处违规会被计两次）。
const RUN = new Set();
for (const [, , fn] of active) {
  if (RUN.has(fn)) continue;
  RUN.add(fn);
  fn();
}

// ── 输出 ──────────────────────────────────────────────────────
const byRule = new Map();
for (const f of findings) {
  if (!byRule.has(f.group)) byRule.set(f.group, []);
  byRule.get(f.group).push(f);
}

if (findings.length === 0) {
  console.log(`DesignPrimitives: clean (${active.map(([id]) => id).join(", ") || "no rules"})`);
  process.exit(0);
}

console.error(`DesignPrimitives: ${findings.length} finding(s)${STRICT ? "" : "  [advisory — 存量未收敛，--strict 才阻断]"}\n`);
for (const [id, desc] of RULES) {
  const list = byRule.get(id);
  if (!list?.length) continue;
  console.error(`  ── ${id} ${desc} —— ${list.length} 处`);
  const shown = list.slice(0, ONLY.length ? 80 : 12);
  for (const f of shown) {
    console.error(`     ${f.file}:${f.line}  ${f.text}`);
  }
  if (list.length > shown.length) {
    console.error(`     … 另有 ${list.length - shown.length} 处`);
  }
  console.error("");
}
console.error(
  `收口路径：先在 advisory 模式清零，再用 --strict 接入 ci-gate.sh 阻断。` +
  `\n（本仓库的纪律是「新增约束优先写进门禁而不是靠自觉」——advisory 只是过渡态。）`,
);

process.exit(STRICT ? 1 : 0);
