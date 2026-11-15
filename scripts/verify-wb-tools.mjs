// 工作台算法工具箱的注入前验证。
//
// 工具箱以 TS 维护（crates/hermix-plugin/src/workbench_tools.rs），注入 iframe 前
// 由 Rust 侧剥类型。剥错一个字符的症状是「Rust 单测全绿、浏览器白屏」——
// 所以这里用真解析器 + 真执行兜底：
//   1. esbuild 解析（语法）
//   2. 动态 import 后跑已知答案的用例（语义）
//
// 用法：
//   WB_TOOLS_DUMP=1 cargo test -p hermix-plugin --lib toolbox
//   node scripts/verify-wb-tools.mjs
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const dump = path.join(root, ".tmp-probe", "stripped.js");

if (!fs.existsSync(dump)) {
  console.error(
    "缺 .tmp-probe/stripped.js —— 先跑：WB_TOOLS_DUMP=1 cargo test -p hermix-plugin --lib toolbox",
  );
  process.exit(1);
}
const code = fs.readFileSync(dump, "utf8");

// ── 1. 语法：交给 esbuild 真解析 ────────────────────────────────
const esbuild = path.join(
  root,
  "node_modules",
  "@esbuild",
  "win32-x64",
  "esbuild.exe",
);
try {
  execFileSync(esbuild, ["--loader=js", "--log-level=warning"], {
    input: code,
    stdio: ["pipe", "pipe", "pipe"],
  });
} catch (e) {
  console.error("✗ 语法不过 —— 这个输出注入 iframe 会白屏：");
  console.error((e.stderr || Buffer.from("")).toString() || e.message);
  process.exit(1);
}
console.log("✓ 语法：esbuild 解析通过");

// ── 2. 语义：真跑一遍，结果对已知答案 ────────────────────────────
const mod = await import(
  "data:text/javascript;base64," + Buffer.from(code).toString("base64")
);

let pass = 0;
const failures = [];
function check(name, actual, expected) {
  if (JSON.stringify(actual) === JSON.stringify(expected)) {
    pass++;
  } else {
    failures.push(
      `${name}\n    期望 ${JSON.stringify(expected)}\n    实得 ${JSON.stringify(actual)}`,
    );
  }
}

check("virtualWindow 空列表", mod.virtualWindow(0, 20, 0, 400), {
  start: 0,
  end: 0,
  padTop: 0,
  padBottom: 0,
  total: 0,
});
check("virtualWindow 滚动中段", mod.virtualWindow(1000, 20, 4000, 400, 4), {
  start: 196,
  end: 224,
  padTop: 3920,
  padBottom: 15520,
  total: 20000,
});

// 行级 diff：中间替换必须识别为 replace，首尾相同行必须保留
{
  const d = mod.diffLines("a\nb\nc", "a\nx\nc\n");
  const types = d.map((h) => h.type);
  check("diffLines 识别替换", types.includes("replace"), true);
  check(
    "diffLines 保留公共首行",
    d[0],
    { type: "equal", leftLine: 1, rightLine: 1, left: "a", right: "a" },
  );
}
check(
  "diffLines 完全相同",
  mod.diffLines("同\n文", "同\n文").every((h) => h.type === "equal"),
  true,
);

// CSV：RFC 4180 的引号转义与引号内换行是手写正则的两个经典坑
check("CSV 引号内逗号", mod.parseDelimited('a,b\n"含,逗号",2'), {
  header: ["a", "b"],
  rows: [["含,逗号", "2"]],
});
check("CSV 引号内换行", mod.parseDelimited('h\n"两\n行"').rows, [["两\n行"]]);
check("CSV 双引号转义", mod.parseDelimited('h\n"双""引"').rows, [['双"引']]);
check("TSV 自定义分隔符", mod.parseDelimited("a\tb\n1\t2", "\t"), {
  header: ["a", "b"],
  rows: [["1", "2"]],
});

check("columnStats 普通数组", mod.columnStats([1, 2, 3, 4]), {
  count: 4,
  sum: 10,
  min: 1,
  max: 4,
  avg: 2.5,
});
check("columnStats 空数组", mod.columnStats([]), {
  count: 0,
  sum: 0,
  min: 0,
  max: 0,
  avg: 0,
});
check("columnStats 忽略非有限数", mod.columnStats([1, NaN, Infinity, 3]), {
  count: 2,
  sum: 4,
  min: 1,
  max: 3,
  avg: 2,
});

if (failures.length) {
  console.error(`✗ 语义：${failures.length} 项失败`);
  for (const f of failures) console.error("  " + f);
  process.exit(1);
}
console.log(`✓ 语义：${pass} 项用例全部通过`);
