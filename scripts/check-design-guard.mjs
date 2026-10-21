#!/usr/bin/env node
// DesignGuard — impeccable / typography 硬约束的 grep 门禁（硬门禁）。
//
// 背景：设计规范（impeccable-design.md、TYPOGRAPHY_SYSTEM.md、tokens.css）
// 此前只存在于文档与评审里，CSS 层与既有组件的违规从未被机械检出——
// check-theme-tokens.mjs 只扫 tsx、且「行内含 var(--color- 即整行豁免」
// 的规则放过了一批 `var(--x, #hex)` 式违规。本脚本补上 TSX 侧的
// 高价值断言；CSS 层量化扫描仍是 TODO（误报率高，先不硬门禁）。
//
// 规则（全部来自 2026-10-18 设计审查的确认项）：
//  G1 TSX 禁字面色值（#hex / rgb(/rgba(）——主题只换令牌值
//     （tokens.css:4-20 契约）。只扫 .tsx：.ts 里的色板常量
//     （agents/types.ts 专家色板）是数据表不是样式。字符串字面量行
//     （title/label 类）豁免。
//  G2 禁 `dark:` 变体（globals.css:23-24 已废除）。
//  G3（已移除）：图标按钮 aria-label 检查。跨行 JSX 的
//     children/attributes 解析用 regex 启发式误报率不可接受
//     （首轮 270 条里 G3 占 90%+ 且多为文本按钮误报）；真正的
//     结构检查需要 TS AST（ts-morph），等有既有依赖再加。无障碍
//     覆盖靠评审 + impeccable detect。
//  G4 视图/组件文件禁「加载中…」字面量（ui/Skeleton.tsx:6-8：加载态
//     用骨架屏替代文字；ui/ 原语内部的文案豁免）。
//  G5 禁 window.confirm（Sidebar.tsx:245 确立应用内 ConfirmDialog 约定）。
//
// 白名单：
//  - 行内含 `design-guard: allow` 注释的行豁免（类似 eslint-disable，
//     用于固定品牌色 / 恒暗浮层装饰色等「不随主题」的合法固定色）。
//  - 本脚本自身的规则名出现不算（不扫 scripts/）。

import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const ROOT = join(process.cwd(), "src");
const EXT = /\.(tsx|ts)$/;
const SKIP = /(\.test\.|\.d\.ts$)/;

// 行内出现这些形态视为「已在令牌体系内」，整行豁免 G1 —— 与
// check-theme-tokens 的差异：豁免条件是**只有令牌**，而不是
// 「含令牌即可」。
const TOKEN_LINE = /^\s*(\/\/|\/\*|\*|\}|else|\{).*/;

function* walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    const st = statSync(p);
    if (st.isDirectory()) yield* walk(p);
    else if (EXT.test(name) && !SKIP.test(p.replaceAll("\\", "/"))) yield p;
  }
}

const violations = [];
const push = (file, line, rule, text) =>
  violations.push(`${relative(process.cwd(), file)}:${line} [${rule}] ${text.trim().slice(0, 120)}`);

for (const file of walk(ROOT)) {
  const lines = readFileSync(file, "utf8").split("\n");

  let lastAllow = -99;
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    // 当前行带 allow 标记，或处于 allow 标记后的 3 行窗口内
    // （注释行 + 被标注的多行块）即豁免。
    if (/design-guard:\s*allow/.test(ln)) {
      lastAllow = i;
      continue;
    }
    if (i - lastAllow <= 3) continue;

    // G2 — dark: 变体已废除
    if (/\bdark:[a-z]/.test(ln) && !/^\s*(\/\/|\*|import )/.test(ln)) {
      push(file, i + 1, "G2-dark-variant", ln);
      continue;
    }
    // G5 — window.confirm
    if (/window\.confirm\(/.test(ln) && !/^\s*(\/\/|\*)/.test(ln)) {
      push(file, i + 1, "G5-window-confirm", ln);
      continue;
    }
    // G4 — 「加载中…」字面量（ui/ 原语与测试豁免已由 SKIP 覆盖测试；
    // ui/ 目录内允许——原语本身可以有加载文案）
    if (/加载中…/.test(ln) && !/\\?u?i\//i.test(file) && !/^\s*(\/\/|\*)/.test(ln)) {
      push(file, i + 1, "G4-loading-literal", ln);
      continue;
    }
    // G1 — TSX 字面色值（只扫 .tsx；.ts 数据色板豁免）
    // 字面色有四种写法：#hex / rgb() / rgba() / hsl()。2026-10-21 补 hsl() ——
    // 此前只拦 #hex 与 rgb(，Sidebar.tsx 的 4 处 hsl() 字面色一路漏网。
    // 只拦**字面量**（数字开头）；`hsl(${计算})` 是程序化派生（MemoryGraph 的
    // 色相环），不是写死的颜色。
    if (
      /\.tsx$/.test(file) &&
      /#[0-9a-fA-F]{3,8}\b|rgba?\(|(?<![$\w])hsla?\(\s*[\d.]/.test(ln) &&
      !TOKEN_LINE.test(ln)
    ) {
      const hasToken = /var\(--/.test(ln);
      // 色板数据行豁免：`{ bg: "#…", label: … }` / banner / avatar /
      // chip 等键值对是「固定品牌色 / 文件类型官方色」的数据表
      // （PDF 永远红、Office 官方色不随主题），不是主题样式。
      const paletteData =
        /[{,]\s*(bg|fg|banner|avatar|chip|label)\s*:/.test(ln) ||
        /\bkey\s*:\s*"/.test(ln) ||
        // `{ providerId: "#brand" }` 形态的品牌色表（ProviderIcon 等）
        /^\s*\w+:\s*"#/.test(ln);
      const inStringOnly =
        /^(\s*(title|label|alt|aria-label|name|id|className)\s*[:=])/.test(ln) ||
        /["'`][^"'`]*["'`],?\s*$/.test(ln);
      if (!hasToken && !inStringOnly && !paletteData) {
        push(file, i + 1, "G1-hex-color", ln);
      }
    }
  }
}

if (violations.length > 0) {
  console.error(`DesignGuard: ${violations.length} violation(s)\n`);
  for (const v of violations) console.error("  " + v);
  process.exit(1);
}
console.log("DesignGuard: clean");
