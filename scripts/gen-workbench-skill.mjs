// 从 workbench_rules.json 生成工作台 SKILL.md。
//
// 规则只有一份真相源（Rust 侧的 workbench_rules.json）：lint 引擎读它做校验，
// 本脚本读它生成给 agent 看的规则文档。两边共用一份，避免「文档 200 行 / lint
// 300 行」的规则漂移——agent 按文档写了却过不了门禁，是最难查的一类错。
//
// 用法: node scripts/gen-workbench-skill.mjs [--out <dir>] [--check]

import { readFileSync, writeFileSync, mkdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { homedir } from "node:os";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const rulesPath = join(root, "crates/hermix-plugin/src/workbench_rules.json");

const args = process.argv.slice(2);
const outIdx = args.indexOf("--out");
const outDir = outIdx >= 0
  ? args[outIdx + 1]
  : join(homedir(), ".hermix/skills/workbench");
const checkOnly = args.includes("--check");

const rules = JSON.parse(readFileSync(rulesPath, "utf8"));

// 规则按 ID 排序，agent 报违规时能按 ID 快速定位到文档段落。
const sorted = [...rules.rules].sort((a, b) => a.id.localeCompare(b.id));

const ruleLines = sorted
  .map((r) => {
    const scope = (r.applies ?? ["*"]).join("/");
    const where = r.onlyOutside
      ? `，仅 ${r.onlyOutside} 之外`
      : r.exceptFiles
        ? `，${r.exceptFiles.join(",")} 除外`
        : "";
    return `- **${r.id}**（${scope}${where}）${r.message}\n  - 修法：${r.fix}`;
  })
  .join("\n");

const locked = rules.lockedFiles.map((f) => `\`${f}\``).join(" 与 ");

const doc = `---
name: workbench
description: 在 Hermix 桌面端创建与迭代「工作台」——用户通过对话生成的个人工作环境。覆盖骨架契约、${sorted.length} 条机器可校验的模块化硬规则（W001 起）与 lint 门禁自修流程、第三方依赖申请、数据与代码分离约定。当对话涉及「建一个工作台」「给工作台加功能」「改工作台界面」时使用。
---

# 工作台（Workbench）开发

工作台是用户在 Hermix 桌面端里通过**一句话**创建、并在**右侧对话栏**里持续演进的个人工作环境。
它不是一个一次性生成的网页：代码在宿主内直接跑（无需打包编译），数据跨会话永久保存。

## 核心立场

**界面长什么样、交互怎么走、功能如何组织，100% 由你决定。** 宿主不提供组件库、
不提供页面模板、不给你 schema 让你填。宿主只做三件事：文件系统、编译器、lint。

换句话说：宿主管的是「**你怎么写**」，不是「**你写什么**」。

## 目录与骨架

工作台根目录：\`~/.hermix/workbenches/<id>/\`

\`\`\`
code/                 代码层 —— 你可以自由创建的唯一区域
  index.html          入口（含宿主维护的 importmap 标记段）
  styles/tokens.css   主题变量（锁定）
  styles/layout.css   布局
  styles/components.css
  src/main.js         入口装配（锁定）
  src/app.js          视图挂载
  src/store.js        唯一状态源
  src/api.js          全仓唯一允许调用 wb.* 的文件
  src/views/          视图模块
  src/components/     纯函数组件
  src/utils/          纯函数工具
data/                 数据层 —— 你的白名单禁区，宿主网关独占
  _undo/              撤销栈
\`\`\`

**骨架是宿主预生成的，你只在格子里填，不在空白页里自由发挥。**
${locked} 由宿主生成并锁定，你写不进去。

## 硬性规则（落盘前 lint 会拦，违规带文件:行号回传给你自修）

以下 ${sorted.length} 条由宿主 lint 引擎机器校验，规则表与本文档同源生成，不会漂移。
（跳号 W010 / W012 / W016–W018 / W020 是设计文档已规划、但需 AST 或上下文分析
才能判定的规则，暂未机器化，不表示你可以违反。）

${ruleLines}

### 体积与重复

- 单文件行数上限：JS \`${rules.limits.js}\` / CSS \`${rules.limits.css}\` / HTML \`${rules.limits.html}\`。
  超了不是让你砍功能，是让你**拆模块**。文件数量没有上限。
- 跨文件重复块 ≥ \`${rules.limits.duplicateBlockLines}\` 行会报 W013 —— 抽到 \`src/utils/\` 或 \`src/components/\`。

## 工作流

1. **看现状**：调 \`workbench_module_index\` 拿到已有模块与导出符号。
   已有 \`formatDate\` 就 import，不要重写一遍。每轮迭代都先看。
2. **写文件**：调 \`workbench_write\` 写入。落盘前宿主自动跑 lint。
3. **收违规**：若返回「文件:行号 + 规则 ID + 修法」，按 ID 查上表改对应行，**只改违规处**，
   不要整篇重写。重写会引入新违规，你会陷入死循环。
4. **预检**（可选但推荐）：批量改动前用 \`workbench_lint\` 带 \`path\`/\`content\` 预检，
   不写脏文件就知道会不会违规。
5. 自修最多 2 轮；超过 2 轮仍违规，说明拆法有问题，重新规划模块边界而不是继续打补丁。

## 第三方库

**鼓励用成熟的开源库，不要重复造轮子。** 但库必须走本地 vendor，不走 CDN。

- 先看 \`workbench_vendor_index\` 有哪些已装库。
- 需要的库没装 → 走依赖申请（宿主把库放进 \`~/.hermix/workbenches/_vendor/<name>/\`
  并登记 \`index.json\`），装好后调 \`workbench_set_deps\` 注入 importmap。
- 注入后即可直接 \`import { Tabulator } from 'tabulator'\`，无需 npm install、无需打包。
- **禁止**从 CDN import（W019）、**禁止**把库源码粘进工作台（W020）、**禁止** \`require\`（W021）。

## 数据与本机能力（\`window.wb.*\`）

全部调用**异步**，返回 Promise。代码层没有 IPC 入口，\`wb\` 是唯一通道，
且**只能在 \`src/api.js\` 里出现**（W007 / W009）。

| 调用 | 用途 |
|---|---|
| \`wb.db.query(sql, params)\` | 只读（SELECT / WITH），返回行对象数组 |
| \`wb.db.exec(sql, params)\` | 写入（CREATE / INSERT / UPDATE / DELETE），返回影响行数 |
| \`wb.fs.read(path)\` / \`wb.fs.write(path, content)\` / \`wb.fs.list(dir)\` | 本机文件 |
| \`wb.emit(event, payload)\` / \`wb.on(event, fn)\` | 与宿主/对话栏双向联动 |

- **参数化查询，不要字符串拼 SQL**；\`ATTACH\` / \`PRAGMA\` / \`VACUUM\` 等由宿主硬拒。
- 数据库在 \`data/workbench.db\`，归宿主网关独占 —— 代码随便改、回滚、重做，数据不动。
- **本机文件直接用，没有授权这一步**：不弹窗、不需要用户预批、不会中断操作。
  只有两类路径不可用，它们不属于"权限"而属于**越权**：
  密钥/凭据类（\`.env\` / \`.ssh\` / \`id_rsa\` / \`credentials\`），
  以及工作台的 \`data/\`（数据层归宿主网关独占，代码层一律绕行）。
  被拒就换路径，别重试。

## 算法工具箱（\`wb-tools\`）

**这四个算法自己写要么几十行、要么写错还难查，直接用宿主的。**
它们全是纯函数，无副作用，不含任何 DOM 结构与样式决策 —— 怎么呈现仍归你。

\`\`\`js
// 方式一（推荐，任意模块可用）：ESM import。
import { virtualWindow, diffLines, parseDelimited, columnStats } from "wb-tools";

// 方式二（同步取用，无需 await）：只在 src/api.js 里写，因为它是 wb.* 命名空间（W007）。
const { diffLines } = wb.util;
\`\`\`

工具箱**不属于用户依赖**：不进 \`workbench_vendor_index\`、不用申请、不用打包。

| 函数 | 签名 | 用途 |
|---|---|---|
| \`virtualWindow\` | \`(itemCount, itemHeight, scrollTop, viewportHeight, overscan=4)\` → \`{start, end, padTop, padBottom, total}\` | 虚拟滚动窗口。**列表超过 ~200 行就该用**：只渲染可见区间，上下用占位高度撑住滚动条 |
| \`diffLines\` | \`(leftText, rightText)\` → \`[{type, leftLine, rightLine, left, right}]\` | 行级 diff。\`type\` 为 \`equal\`/\`insert\`/\`delete\`/\`replace\`。空间 O(min(n,m))，10 万行不炸内存 |
| \`parseDelimited\` | \`(text, delimiter=",")\` → \`{header, rows}\` | CSV/TSV 解析。**正确处理 RFC 4180 引号转义（\`""\`）与引号内换行** —— 这两点手写正则几乎必错 |
| \`columnStats\` | \`(values)\` → \`{count, sum, min, max, avg}\` | 数值列统计，自动忽略 \`NaN\`/\`Infinity\` |

**别做的事**：不要为了这几件事引第三方库（\`papa-parse\` / \`diff\` 之类）；
不要在 \`src/utils/\` 里再写一份同名实现 —— W013 重复检测会报，而且那是纯浪费。

## 运营边界：改数据，不改代码

右侧对话栏有两种请求：**改工作台**（开发）和**替用户干活**（运营）——「把这周的账记一下」
「每天 8 点汇总昨日数据」是运营，不是开发。两者的分界：

| | 开发 | 运营 |
|---|---|---|
| 改什么 | \`code/\` 下的文件 | \`data/\` 里的业务数据 |
| 工具 | \`workbench_write\` / \`workbench_read\` | 数据网关（SQL / fs，见「业务数据现状」注入段） |
| 生效 | lint 门禁 + 热更新 | 数据即时生效 |

**运营动作一律不调用 \`workbench_write\`。** 若用户的请求实际需要新功能才能完成
（如「每天自动导出」需要新模块），明说「这需要加个功能」，转开发流程——
不要用运营通道偷偷写代码。改表结构（CREATE/ALTER TABLE）也属开发，不走运营。

定时任务：用户要「每天/每周自动做某事」时，用 \`cronjob\` 工具注册，
schedule 用 cron 表达式，prompt 写清要做的事；注册前跟用户确认时间与内容。

## 与用户对话

右侧对话栏是宿主渲染的 chrome，**你无权隐藏或改动它**，用户可自行折叠。
界面与对话双向联动：用户在界面上操作可以 \`wb.emit(event, payload)\` 唤起你来改。

改完直接告诉用户改了什么、怎么试，不要输出大段代码回显。
`;

const target = join(outDir, "SKILL.md");

if (checkOnly) {
  if (!existsSync(target)) {
    console.error(`missing: ${target}`);
    process.exit(1);
  }
  const current = readFileSync(target, "utf8");
  if (current !== doc) {
    console.error(`stale: ${target} — run: node scripts/gen-workbench-skill.mjs`);
    process.exit(1);
  }
  console.log("workbench SKILL.md is up to date");
  process.exit(0);
}

mkdirSync(outDir, { recursive: true });
writeFileSync(target, doc);
console.log(`wrote ${target} (${sorted.length} rules)`);
