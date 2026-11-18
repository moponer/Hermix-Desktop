#!/usr/bin/env node
// B4-4（docs/notes/proposed/architecture/2026-11-16-optimization-plan.md H5）：基准评分脚本骨架。
//
// 输入契约：JSONL，每行一条 LLM 调用记录，至少包含：
//   { prompt_tokens, completion_tokens, cache_read_tokens?, task?, role?, status? }
// 缺失可选字段的行按未知桶聚合，不报错。
//
// 用法：
//   node score.mjs --usage <usage.jsonl> [--baseline baseline.json] [--out baseline.json]
//
// 规则（对照方案 H5）：
// - 所有指标从 usage 重算，禁止手工填；
// - 任一指标相对 baseline 退化 >5% → exit 1（阻止合并）；
// - --out 给出时把本次结果写回 baseline（人工确认有意的变更后执行）。

import { readFileSync, writeFileSync, existsSync } from 'node:fs';

const args = process.argv.slice(2);
function argOf(flag) {
  const i = args.indexOf(flag);
  return i >= 0 ? args[i + 1] : null;
}

const usagePath = argOf('--usage');
const baselinePath = argOf('--baseline') ?? 'scripts/bench/baseline.json';
const outPath = argOf('--out');

if (!usagePath || !existsSync(usagePath)) {
  console.error('用法：node score.mjs --usage <usage.jsonl> [--baseline baseline.json] [--out baseline.json]');
  console.error('缺少 --usage 或文件不存在。');
  process.exit(2);
}

// ── 采集 �───────────────────────────────────────────────────────────────
const rows = readFileSync(usagePath, 'utf8')
  .split('\n')
  .filter((l) => l.trim())
  .map((l) => {
    try {
      return JSON.parse(l);
    } catch {
      return null; // 崩溃留下的半行——跳过，与读侧策略一致
    }
  })
  .filter(Boolean);

const num = (v) => (typeof v === 'number' && Number.isFinite(v) ? v : 0);
const tasks = new Set();
let tokensTotal = 0;
let promptTotal = 0;
let cacheReadTotal = 0;
let exits = 0;
let firstRunSuccess = 0;
let errorExits = 0;
const perTask = new Map();

for (const r of rows) {
  tokensTotal += num(r.prompt_tokens) + num(r.completion_tokens);
  promptTotal += num(r.prompt_tokens);
  cacheReadTotal += num(r.cache_read_tokens);
  if (r.task) {
    tasks.add(r.task);
    const t = perTask.get(r.task) ?? { tokens: 0, exits: 0, errors: 0 };
    t.tokens += num(r.prompt_tokens) + num(r.completion_tokens);
    perTask.set(r.task, t);
  }
  if (r.status) {
    exits += 1;
    if (r.status === 'reported') firstRunSuccess += 1;
    if (String(r.status).startsWith('error') || r.status === 'failed') errorExits += 1;
  }
}

const cacheHitRatio = promptTotal > 0 ? cacheReadTotal / promptTotal : null;
const metrics = {
  tasks_total: tasks.size,
  first_run_success_rate: exits > 0 ? firstRunSuccess / exits : null,
  error_exit_ratio: exits > 0 ? errorExits / exits : null,
  rework_rounds_avg: null, // 需要任务卷宗的 rejectRounds——采集面齐后接上
  tokens_per_task_avg: tasks.size > 0 ? Math.round(tokensTotal / tasks.size) : null,
  tokens_total: tokensTotal,
  cache_hit_ratio: cacheHitRatio === null ? null : Number(cacheHitRatio.toFixed(4)),
  sub_agent_reject_ratio: null, // 需要 delegation_attempts.jsonl——接上后填
  sandbox_violation_count: null, // 需要审计事件流——接上后填
};

// ── 对比基线（退化 >5% 阻止合并）─────────────────────────────────────
let baseline = null;
if (existsSync(baselinePath)) {
  try {
    baseline = JSON.parse(readFileSync(baselinePath, 'utf8')).metrics ?? null;
  } catch {
    baseline = null;
  }
}

let regression = false;
if (baseline) {
  for (const [k, v] of Object.entries(metrics)) {
    const b = baseline[k];
    if (b === null || v === null || b === 0) continue;
    // 越大越好的指标（成功率/命中率）允许下降；成本/轮次类只看上涨
    const higherIsBetter = k.endsWith('_rate') || k.endsWith('_ratio') && k !== 'error_exit_ratio';
    const delta = (v - b) / b;
    if (!higherIsBetter && delta > 0.05) {
      console.error(`✗ ${k} 退化：${b} → ${v}（+${(delta * 100).toFixed(1)}% > 5%）`);
      regression = true;
    } else if (higherIsBetter && delta < -0.05) {
      console.error(`✗ ${k} 退化：${b} → ${v}（${(delta * 100).toFixed(1)}% < -5%）`);
      regression = true;
    }
  }
}

console.log(JSON.stringify({ metrics, perTask: Object.fromEntries(perTask) }, null, 2));

if (outPath) {
  writeFileSync(outPath, JSON.stringify({ version: 1, capturedAt: new Date().toISOString(), metrics, perTask: Object.fromEntries(perTask) }, null, 2) + '\n');
  console.log(`已写出：${outPath}`);
}

process.exit(regression ? 1 : 0);
