/**
 * smoke-message-parser-banner.cjs
 *
 * 端到端 smoke test：锁住 [CONTEXT COMPACTION] banner 不再泄漏到用户 UI。
 *
 * 历史背景（2026-06-27 主人反馈 "恢复对话时 banner 在屏幕上"）：
 *   - MessageBubble 走 parseContent → strip ✓
 *   - StreamingBubble 直接把流式文本喂 MarkdownBody → strip ✗（LLM 回放 banner 时可见）
 *
 * 本测试覆盖两条路径的最终 strip 结果 + 4 种 banner 变体（带 END marker / 不带 /
 * LLM 前面加引导语 / LLM 后面接续写）。
 *
 * 用法：node scripts/smoke-message-parser-banner.cjs
 * 退出码：0 通过，非 0 失败。
 */

const path = require('path');
const fs = require('fs');

// 直接 import 源 TS 文件，用 tsx 不可用时的退化方案：复制 regex 字符串 + 解析逻辑。
// 这样比 dynamic import TS 文件更稳（避免 tsx/ts-node 依赖）—
// 任何对 messageParser.ts 的 regex 改动会让此 smoke 落后，需要同步更新本测试。
const SRC = path.join(__dirname, '..', 'src', 'components', 'messageParser.ts');
const src = fs.readFileSync(SRC, 'utf8');

// 抽 regex 常量（多行安全）：找出所有 `const NAME_RE = /.../flags;` 块
const reConsts = {};
const reDecl = /\b(?:const|let)\s+(\w+_RE)\s*=\s*(\/(?:\\.|[^/\\])+\/[gimuy]*)/g;
let m;
while ((m = reDecl.exec(src)) !== null) {
  const name = m[1];
  const lit = m[2]; // /pattern/flags
  const lastSlash = lit.lastIndexOf('/');
  const pattern = lit.slice(1, lastSlash);
  const flags = lit.slice(lastSlash + 1);
  reConsts[name] = new RegExp(pattern, flags);
}

const required = ['THINKING_RE', 'ORPHAN_OPEN_RE', 'LIFECYCLE_RE', 'CONTEXT_COMPACTION_RE',
                  'TOOL_CALL_FULL_RE', 'TOOL_CALL_BARE_OPEN_RE', 'INVOKE_BLOCK_RE',
                  'ORPHAN_CLOSE_RE', 'BARE_MINIMAX_LITERAL_RE', 'CHECKBOX_RE'];
for (const k of required) {
  if (!reConsts[k]) {
    console.error(`[smoke] FAIL: messageParser.ts 缺少 ${k}`);
    process.exit(1);
  }
}

// 复刻 parseContent 行为（仅 regex 部分；tasks 提取非本次测试目标）
function strip(content) {
  let working = content;
  working = working.replace(reConsts.THINKING_RE, '');
  if (working.match(reConsts.ORPHAN_OPEN_RE)) {
    working = working.replace(reConsts.ORPHAN_OPEN_RE, '');
  }
  working = working.replace(reConsts.LIFECYCLE_RE, '');
  working = working.replace(reConsts.CONTEXT_COMPACTION_RE, '');
  working = working.replace(reConsts.TOOL_CALL_FULL_RE, '');
  working = working.replace(reConsts.TOOL_CALL_BARE_OPEN_RE, '$1');
  working = working.replace(reConsts.INVOKE_BLOCK_RE, '');
  working = working.replace(reConsts.ORPHAN_CLOSE_RE, '');
  working = working.replace(reConsts.BARE_MINIMAX_LITERAL_RE, '$1');
  return working.replace(reConsts.CHECKBOX_RE, '').trim();
}

// 模拟 backend 实际注入的 fallback banner（与 crates/hermix-agent/src/compression.rs
// 的 SUMMARY_PREFIX + 静态 fallback + SUMMARY_END_MARKER 一致）
const FALLBACK_BANNER = [
  '[CONTEXT COMPACTION] Earlier turns in this conversation were compacted to save context space.',
  'The summary below describes work that was already completed, and the current session state',
  'may still reflect that work (for example, files may already be changed). Use the summary and',
  'the current state to continue from where things left off, and avoid repeating work.',
  '',
  'IMPORTANT — read before responding:',
  '• Do NOT echo, quote or re-issue any inline placeholders you see in the recent messages',
  '  below (e.g. @image#1, @file#2, image#1: ...). Those were inline attachments handled by',
  '  the UI and have already been consumed. If you reproduce them, the user will see them',
  '  again as if you just produced them.',
  '• Do NOT pretend any browser tool call is still \'running\'. Treat every browser_* tool',
  '  result in the recent messages as already completed; the original tool emitter has',
  '  already moved on.',
  '• This banner is a system-level metadata marker, not a user turn. Never treat it as a',
  '  new question or task.',
  'Summary generation was unavailable. 13 conversation turns were removed to free context',
  'space but could not be summarized. The removed turns contained earlier work in this',
  'session. Continue based on the recent messages below and the current state of any files',
  'or resources.',
  '',
  '--- END OF CONTEXT SUMMARY — respond to the message below, not the summary above ---',
].join('\n');

// LLM 真成功生成摘要时（带 END marker 的 SUMMARY_PREFIX + summary text）
const LIVE_BANNER = [
  '[CONTEXT COMPACTION] Earlier turns in this conversation were compacted to save context space.',
  'The summary below describes work that was already completed.',
  '',
  '<Goal>用户要把 backup script 改成 systemd timer</Goal>',
  '<Progress>已查到当前 cron 配置、确认 systemd-timer 写法</Progress>',
  '<Next>写 unit file</Next>',
  '',
  '--- END OF CONTEXT SUMMARY — respond to the message below, not the summary above ---',
].join('\n');

// REFERENCE ONLY 变体（无 END marker，不在本测试路径内 — 注释说明）
const REFERENCE_ONLY =
  '[CONTEXT COMPACTION — REFERENCE ONLY] This session was reloaded from disk; the original system prompt and 14 turns were stripped. Continue based on the user\'s most recent message.';

const tests = [
  {
    name: 'fallback banner (SUMMARY_PREFIX + 静态文本 + END marker) → 完全剥离',
    input: FALLBACK_BANNER,
    expectBodyEmpty: true,
  },
  {
    name: 'live banner (SUMMARY_PREFIX + LLM 摘要 + END marker) → 完全剥离',
    input: LIVE_BANNER,
    expectBodyEmpty: true,
  },
  {
    name: 'LLM 回放 banner 并加一句续写 → 只剩续写',
    input: FALLBACK_BANNER + '\n\nOK let me continue from where I left off.',
    expectBodyEmpty: false,
    expectContains: 'OK let me continue',
    expectNotContains: '[CONTEXT COMPACTION]',
  },
  {
    name: 'LLM 前面加引导语 + banner → 只剩引导语',
    input: 'Got it, continuing:\n\n' + FALLBACK_BANNER,
    expectBodyEmpty: false,
    expectContains: 'Got it, continuing',
    expectNotContains: '[CONTEXT COMPACTION]',
  },
  {
    name: 'LLM echo 前缀缺失 ([CONTEXT COMPACTION] 被截断) → 安全降级（不破坏用户内容）',
    input: '[CONTEXT COMPACTION was here but got truncated mid-stream...]\n继续工作。',
    expectBodyEmpty: false,
    expectContains: '继续工作',
    // 这个 case 里 strip regex 不匹配（缺 END marker），整段原样保留 — 这是
    // messageParser.ts 注释里说的"false negative acceptable"语义。
  },
  {
    name: '正常中文用户消息 → 不受影响',
    input: '请帮我把 backup script 改成 systemd timer',
    expectBodyEmpty: false,
    expectContains: '请帮我把 backup script',
  },
  {
    name: '<think> 块剥离 (主对话模型 Claude/Qwen → MiniMax 残留)',
    input: '<think>\nThe user wants X.\n</think>\n好的，我来帮你。',
    expectBodyEmpty: false,
    expectNotContains: '<think>',
    expectContains: '好的，我来帮你',
  },
];

let passed = 0;
let failed = 0;

for (const t of tests) {
  const got = strip(t.input);
  let ok = true;
  let reason = [];

  if (t.expectBodyEmpty && got.length > 0) {
    ok = false;
    reason.push(`期望 body 完全剥离，实际长度 ${got.length}: ${JSON.stringify(got.slice(0, 120))}`);
  }
  if (!t.expectBodyEmpty && got.length === 0) {
    ok = false;
    reason.push(`期望保留正文，实际为空`);
  }
  if (t.expectContains && !got.includes(t.expectContains)) {
    ok = false;
    reason.push(`期望包含 "${t.expectContains}"，未找到`);
  }
  if (t.expectNotContains && got.includes(t.expectNotContains)) {
    ok = false;
    reason.push(`期望不含 "${t.expectNotContains}"，实际包含`);
  }

  if (ok) {
    console.log(`  ✓ ${t.name}`);
    passed++;
  } else {
    console.log(`  ✗ ${t.name}`);
    console.log(`    input (前 200): ${JSON.stringify(t.input.slice(0, 200))}`);
    console.log(`    output: ${JSON.stringify(got)}`);
    for (const r of reason) console.log(`    → ${r}`);
    failed++;
  }
}

console.log(`\n[smoke] ${passed} passed, ${failed} failed`);
if (failed > 0) process.exit(1);