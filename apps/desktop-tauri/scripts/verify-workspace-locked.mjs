#!/usr/bin/env node
// verify-workspace-locked.mjs
//
// 2026-06-26: 主人反馈"目录进入对话界面后就无法设置了，在进入前没有设置过"。
// 根因：ChatView.tsx 在两个分支（欢迎态 line 1424 / 活跃 session 态 line 1759）
//   给 `<Composer>` 传了不同的 workspaceLocked 判定：
//     - 欢迎态：写死 `false`（无问题）
//     - 活跃 session 态：`Boolean(session?.messages?.length)` —— 因为这个分支
//       已经筛了 messages>0，所以永远 true = 永久锁死
//   判定标准应该是"用户是否设过 workspace"（`Boolean(workspace)`），与
//   useMemo workspaceLocked（line 1036-1044 改后）共享同一真相源。
//
// 这个脚本做两件事：
//   1. 复刻 ChatView 改后的判定逻辑，断言 4 个场景（不依赖 React/Vite）
//   2. grep ChatView.tsx 确认两个 workspaceLocked={...} 表达式都改成
//      `Boolean(workspace)`，且旧表达式已彻底消失（防残存）
//
// 用法：node apps/desktop-tauri/scripts/verify-workspace-locked.mjs
// 退出码：0 = 全部 PASS；1 = 任一 FAIL

import fs from "node:fs";
import path from "node:path";
import url from "node:url";

const __dirname = path.dirname(url.fileURLToPath(import.meta.url));
const CHATVIEW = path.resolve(__dirname, "..", "src", "components", "ChatView.tsx");

// ── 1. 复刻 ChatView 改后的判定逻辑 ─────────────────────────────────
/**
 * Reference implementation — must stay in lockstep with ChatView.tsx.
 * The real source is in:
 *   - useMemo at line 1036-1044 (useMemo(() => Boolean(workspace), [workspace]))
 *   - <Composer> at line 1424 + 1759 (workspaceLocked={Boolean(workspace)})
 */
function computeWorkspaceLocked({ workspace, isWelcome, sessionHasMessages }) {
  // 修后逻辑：永远按"是否设过 workspace"判定，与两个分支的渲染 prop 共享。
  // 欢迎态（!session || messages.length === 0）也按这个语义走：
  //   - 没设过 → unlocked → 可以从欢迎态挑目录
  //   - 设过 → locked（虽然这与"还没发过消息"的用户预期不太吻合，但与
  //     主人原始预期"设过就不能设"一致；新对话的 sessionId 全新，workspace
  //     state 默认 null，所以这条路径在新建场景下不触发）
  return Boolean(workspace);
}

const cases = [
  {
    name: "活跃 session + 从未设过 workspace（主人报的 bug 场景）",
    args: { workspace: null, isWelcome: false, sessionHasMessages: true },
    expect: false,
    why: "这是主人原话 '进入对话界面后在进入前没有设置过' 的精确复刻。修后应 unlocked。",
  },
  {
    name: "活跃 session + 设过 workspace（预期锁死）",
    args: { workspace: "C:/Users/me/project", isWelcome: false, sessionHasMessages: true },
    expect: true,
    why: "这是主人原话 '除非设置过了就不能设置了' 的精确复刻。",
  },
  {
    name: "欢迎态 + 从未设过（新建对话）",
    args: { workspace: null, isWelcome: true, sessionHasMessages: false },
    expect: false,
    why: "新建对话 → workspace 默认 null → unlocked → 用户可挑目录。",
  },
  {
    name: "欢迎态 + 设过 workspace（用户从欢迎态主动选过目录）",
    args: { workspace: "D:/work", isWelcome: true, sessionHasMessages: false },
    expect: true,
    why: "用户已经在欢迎态选过目录了 → 锁住，避免后续误改。",
  },
];

let logicFails = 0;
console.log("\n[1/2] 逻辑断言（4 场景）");
for (const c of cases) {
  const got = computeWorkspaceLocked(c.args);
  const ok = got === c.expect;
  const tag = ok ? "PASS" : "FAIL";
  console.log(`  ${tag}  ${c.name}`);
  console.log(`        workspace=${JSON.stringify(c.args.workspace)} isWelcome=${c.args.isWelcome} sessionHasMessages=${c.args.sessionHasMessages}`);
  console.log(`        期望 locked=${c.expect}  实得 locked=${got}  // ${c.why}`);
  if (!ok) logicFails += 1;
}

// ── 2. grep ChatView.tsx 确认源代码已修对 ───────────────────────────
console.log("\n[2/2] 源码 grep（确认两处 workspaceLocked prop 都改对，且旧判定无残留）");
const src = fs.readFileSync(CHATVIEW, "utf8");
const lines = src.split("\n");

const newPropMatches = [];
const oldPropMatches = [];
const newMemoMatches = [];
const oldMemoMatches = [];

lines.forEach((line, i) => {
  const ln = i + 1;
  if (/workspaceLocked=\{Boolean\(workspace\)\}/.test(line)) {
    newPropMatches.push(ln);
  }
  if (/workspaceLocked=\{Boolean\(session\?\.messages\?\.length\)\}/.test(line)) {
    oldPropMatches.push(ln);
  }
  if (/useMemo\(\(\) => Boolean\(workspace\)/.test(line)) {
    newMemoMatches.push(ln);
  }
  // 旧 useMemo 的特征：依赖 [sessionId, workspace] 且内部读 localStorage
  if (/localStorage\.getItem\(`hermix\.session\.\$\{sessionId\}\.workspace`\)/.test(line)) {
    oldMemoMatches.push(ln);
  }
});

let grepFails = 0;
function check(label, cond, detail) {
  const tag = cond ? "PASS" : "FAIL";
  console.log(`  ${tag}  ${label}${detail ? `  // ${detail}` : ""}`);
  if (!cond) grepFails += 1;
}

check(
  "两处 <Composer workspaceLocked={Boolean(workspace)}> 都已替换（welcome 1424 + active 1759）",
  newPropMatches.length >= 2,
  `命中行：${newPropMatches.join(", ") || "(无)"}`,
);
check(
  "旧 workspaceLocked={Boolean(session?.messages?.length)} 已彻底删除",
  oldPropMatches.length === 0,
  `残留行：${oldPropMatches.join(", ") || "(无)"}`,
);
check(
  "useMemo 改为 useMemo(() => Boolean(workspace), [workspace])",
  newMemoMatches.length >= 1,
  `命中行：${newMemoMatches.join(", ") || "(无)"}`,
);
check(
  "useMemo 内不再读 localStorage（不再把 localStorage 当作锁源）",
  oldMemoMatches.length === 0,
  `残留行：${oldMemoMatches.join(", ") || "(无)"}`,
);

const totalFails = logicFails + grepFails;
console.log(`\n=== 总结：${totalFails === 0 ? "ALL PASS ✓" : `${totalFails} FAIL ✗`}（逻辑 ${4 - logicFails}/4，源码 ${4 - grepFails}/4）===\n`);
process.exit(totalFails === 0 ? 0 : 1);
