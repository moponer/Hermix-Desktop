#!/usr/bin/env node
// P1-3 smoke: verify the /insights usage aggregation surface is wired.
// Static smoke — it asserts the schema migration, aggregate functions,
// Rust tests, and frontend SVG surface all exist in source, so a
// refactor can't silently regress token usage / daily curve / by_tool.

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
let failed = 0;
function check(name, cond, hint) {
  const status = cond ? 'OK ' : 'FAIL';
  console.log(`[${status}] ${name}${hint ? '  — ' + hint : ''}`);
  if (!cond) failed++;
}

// 1. token_usage table schema added to session_persistence.
const spPath = path.join(ROOT, 'crates/hermix-agent/src/session_persistence.rs');
const spSrc = fs.readFileSync(spPath, 'utf8');
check(
  'token_usage table created in schema',
  /CREATE TABLE IF NOT EXISTS token_usage/.test(spSrc),
  'append-only LLM usage table'
);
check(
  'token_usage indexes created',
  /idx_token_usage_created_at/.test(spSrc) && /idx_token_usage_model/.test(spSrc),
  'created_at + model + session indexes'
);
check(
  'persist_token_usage function exists',
  /pub fn persist_token_usage\(/.test(spSrc),
  'public API for token row inserts'
);
check(
  'UsageAggregate has new token + daily + by_tool fields',
  /pub total_input_tokens: u64/.test(spSrc) &&
    /pub daily: Vec<DailyUsageRow>/.test(spSrc) &&
    /pub by_tool: Vec<ToolUsageRow>/.test(spSrc),
  'aggregate surface extension'
);
check(
  'aggregate_usage left-joins token_usage on by_model',
  /LEFT JOIN token_usage/.test(spSrc),
  'real per-model token totals'
);
check(
  'aggregate_usage materializes daily buckets',
  /materialise the full window/.test(spSrc) || /daily\.push\(DailyUsageRow/.test(spSrc),
  'days+1 buckets so missing days show as 0'
);
check(
  'aggregate_tools helper exists',
  /fn aggregate_tools\(\s*\n?\s*&self,[\s\S]*?conn: &rusqlite::Connection[\s\S]*?cutoff_str: &str/.test(spSrc),
  'private helper that parses messages.tool_calls JSON'
);
check(
  'aggregate_tools handles legacy function.name envelope',
  /tc\.get\("function"\)/.test(spSrc),
  'OpenAI-style JSON fallback'
);
check(
  'daily curve ordered oldest-first for SVG',
  /Reverse so oldest is index 0/.test(spSrc),
  'left-to-right reading direction'
);
check(
  'by_tool uses trailing 7-day window for daily_avg',
  /trailing7_cutoff/.test(spSrc) && /trailing_counts/.test(spSrc),
  'current-frequency average, not lifetime noise'
);

// 2. agent_loop.rs persists token_usage at finalize.
const alPath = path.join(ROOT, 'crates/hermix-agent/src/agent_loop.rs');
const alSrc = fs.readFileSync(alPath, 'utf8');
check(
  'agent_loop persists token_usage in finalize_agent_result',
  /P1-3.*persist the final accumulated token usage/s.test(alSrc) &&
    /persist_token_usage/.test(alSrc),
  'finalize-time hook wires session → token_usage row'
);

// 3. analytics endpoint returns daily + by_tool.
const anPath = path.join(ROOT, 'crates/hermix-server/src/dashboard/analytics.rs');
const anSrc = fs.readFileSync(anPath, 'utf8');
check(
  'analytics endpoint maps by_tool',
  /\.by_tool[\s\S]+?\.into_iter/.test(anSrc) && /AnalyticsToolEntry/.test(anSrc),
  'real tool counts forwarded to API consumer'
);
check(
  'analytics endpoint maps daily curve',
  /\.daily[\s\S]+?\.into_iter/.test(anSrc) && /AnalyticsDailyEntry/.test(anSrc),
  'real daily breakdown forwarded'
);
check(
  'analytics endpoint maps total_input/total_output',
  /total_input: agg\.total_input_tokens/.test(anSrc) &&
    /total_output: agg\.total_output_tokens/.test(anSrc),
  'no more "0 tokens" placeholder'
);

// 4. dashboard types carry the new fields.
const typesPath = path.join(ROOT, 'crates/hermix-server/src/dashboard/types.rs');
const typesSrc = fs.readFileSync(typesPath, 'utf8');
check(
  'AnalyticsToolEntry struct defined',
  /pub struct AnalyticsToolEntry/.test(typesSrc) &&
    /pub tool: String/.test(typesSrc) &&
    /pub call_count: u64/.test(typesSrc),
  'API DTO'
);
check(
  'AnalyticsResponse.by_tool added',
  /pub by_tool: Vec<AnalyticsToolEntry>/.test(typesSrc),
  'response shape extends without breaking old consumers (Vec default empty)'
);

// 5. Frontend API types include by_tool.
const apiPath = path.join(ROOT, 'apps/desktop-tauri/src/api.ts');
const apiSrc = fs.readFileSync(apiPath, 'utf8');
check(
  'frontend AnalyticsToolEntry type defined',
  /export interface AnalyticsToolEntry/.test(apiSrc) &&
    /call_count: number/.test(apiSrc),
  'TS mirror of Rust DTO'
);
check(
  'frontend AnalyticsResponse.by_tool added',
  /by_tool: AnalyticsToolEntry\[\]/.test(apiSrc),
  'response shape extension in TS'
);

// 6. Frontend AnalyticsView renders SVG daily curve.
const viewPath = path.join(ROOT, 'apps/desktop-tauri/src/components/AnalyticsView.tsx');
const viewSrc = fs.readFileSync(viewPath, 'utf8');
check(
  'AnalyticsView renders SVG daily curve',
  /<svg viewBox=/.test(viewSrc) && /fill-blue-400/.test(viewSrc),
  'pure SVG, zero deps, blue=input + green=output + purple=sessions'
);
check(
  'AnalyticsView renders by_tool top 10 list',
  /by_tool\.slice\(0, 10\)/.test(viewSrc) && /Wrench/.test(viewSrc),
  'top tools bar list'
);
check(
  'AnalyticsView handles legacy zero-token data',
  /hasRealTokens/.test(viewSrc) && /token 数据从 2026-06-27 起收集/.test(viewSrc),
  'migration hint for pre-P1-3 databases'
);

// 7. Unit tests cover the new aggregation paths.
check(
  'P1-3 unit tests exist in session_persistence',
  /fn p1_3_persist_token_usage_basic/.test(spSrc) &&
    /fn p1_3_aggregate_usage_daily_fills_missing_days_with_zero/.test(spSrc) &&
    /fn p1_3_by_tool_counts_flat_and_legacy_shapes/.test(spSrc) &&
    /fn p1_3_by_tool_sorts_descending_by_count/.test(spSrc) &&
    /fn p1_3_aggregate_usage_empty_db_returns_zero_shape/.test(spSrc) &&
    /fn p1_3_by_model_includes_token_totals_via_left_join/.test(spSrc),
  '6 unit tests covering token + daily + by_tool + sort'
);

console.log(failed === 0 ? '\nALL OK' : `\n${failed} FAILED`);
process.exit(failed === 0 ? 0 : 1);