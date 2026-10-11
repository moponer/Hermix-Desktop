/**
 * 按行精确删除"100% 死重复 CSS 块"。
 *
 * 仅删块（选择器 + {…}），不动相邻注释、不动文件其余内容 —— 最小 diff。
 * 块范围由 postcss 的 source.start.line / source.end.line 精确定位（理解
 * 注释与字符串，比裸括号计数可靠）。同一文件多个目标按 end 行降序 splice，
 * 避免删一处后行号偏移。
 *
 * 跑法：node scripts/remove-dead-css.cjs
 * 安全前提：目标块的所有声明已被后续同选择器（globals 内部同步 @import，
 * 级联顺序确定）规则完全覆盖 —— 删除后 computed style 不变。
 */

const fs = require("fs");
const path = require("path");
const postcss = require("postcss");

const ROOT = path.resolve(__dirname, "..", "src", "styles");

// 安全目标（排除被 inline-approval-card.css 覆盖的 5 处 .approval-btn*）。
const TARGETS = [
  { file: "components-controls.css", line: 24 }, // .btn:disabled  <- helpers
  { file: "components-controls.css", line: 299 }, // .modal-footer    <- helpers
  { file: "components-prose.css", line: 605 }, // .preview-pane           <- tool-card
  { file: "components-prose.css", line: 627 }, // .preview-pane-inner pre  <- tool-card
  { file: "components-prose.css", line: 660 }, // .preview-filename...     <- tool-card
  { file: "components-prose.css", line: 667 }, // .file-pill-toggle        <- tool-card
  { file: "components-prose.css", line: 680 }, // .file-pill-toggle:hover   <- tool-card
  { file: "components-tool-card.css", line: 163 }, // .tw-msg-output-copy:hover <- helpers
  { file: "components-tool-card.css", line: 306 }, // .tw-msg-icon              <- tool-card(后)
  { file: "components-tool-card.css", line: 316 }, // .tw-msg-row[run] .tw-msg-icon <- tool-card
  { file: "components-tool-card.css", line: 317 }, // .tw-msg-row[ok]  .tw-msg-icon <- tool-card
  { file: "components-tool-card.css", line: 318 }, // .tw-msg-row[err] .tw-msg-icon <- tool-card
  { file: "components-tool-card.css", line: 1138 }, // .apv-btns               <- tool-card(后)
  { file: "components-helpers.css", line: 1575 }, // .app-detail-code__editor <- helpers(后)
];

// 按文件分组
const byFile = new Map();
for (const t of TARGETS) {
  if (!byFile.has(t.file)) byFile.set(t.file, []);
  byFile.get(t.file).push(t.line);
}

let totalRemoved = 0;
for (const [file, lines] of byFile) {
  const fp = path.join(ROOT, file);
  const css = fs.readFileSync(fp, "utf8");
  const root = postcss.parse(css, { from: fp });
  const spans = [];
  root.walkRules((rule) => {
    if (lines.includes(rule.source.start.line)) {
      spans.push({
        s: rule.source.start.line,
        e: rule.source.end.line,
        sel: rule.selector,
      });
    }
  });
  if (spans.length === 0) {
    console.error("WARN no rule matched in", file, lines);
    continue;
  }
  // 去重（同一 line 可能拆成多条 selector 记录）
  const uniq = [];
  const seen = new Set();
  for (const sp of spans) {
    const key = sp.s + "-" + sp.e;
    if (!seen.has(key)) {
      seen.add(key);
      uniq.push(sp);
    }
  }
  uniq.sort((a, b) => b.e - a.e); // 降序
  const arr = css.split("\n");
  for (const sp of uniq) {
    // 删除 [s-1 .. e-1]，并顺手吃掉紧随其后的连续空行（最多留 1 行）
    let end = sp.e - 1;
    while (
      end + 1 < arr.length &&
      arr[end + 1].trim() === "" &&
      (end + 2 >= arr.length || arr[end + 2].trim() !== "")
    ) {
      end++;
    }
    const removed = arr.splice(sp.s - 1, end - (sp.s - 1) + 1);
    totalRemoved += removed.length;
    console.log(
      `removed ${file}:${sp.s}-${sp.e} (${removed.length} 行)  ${sp.sel.slice(0, 50)}`
    );
  }
  fs.writeFileSync(fp, arr.join("\n"), "utf8");
}

console.log("\n共删除 " + totalRemoved + " 行（" + TARGETS.length + " 个候选块）。");
