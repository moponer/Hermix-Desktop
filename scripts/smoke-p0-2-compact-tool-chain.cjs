#!/usr/bin/env node
// P0-2 smoke: verify the compact_tool_chain surface is wired and safe.
// Static smoke — it asserts the tool module, registration, schema,
// and system-prompt wiring exist in source, so a refactor can't
// silently regress the zero-context RPC contract.

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
let failed = 0;
function check(name, cond, hint) {
  const status = cond ? 'OK ' : 'FAIL';
  console.log(`[${status}] ${name}${hint ? '  — ' + hint : ''}`);
  if (!cond) failed++;
}

// 1. tool_chain module exists and exports the handler.
const modPath = path.join(ROOT, 'crates/hermix-tools/src/tools/tool_chain.rs');
const modSrc = fs.readFileSync(modPath, 'utf8');
check(
  'tool_chain.rs declares CompactToolChainHandler',
  modSrc.includes('pub struct CompactToolChainHandler'),
  'public handler struct'
);
check(
  'tool_chain.rs implements ToolHandler for handler',
  /impl ToolHandler for CompactToolChainHandler/.test(modSrc),
  'handler implements ToolHandler'
);
check(
  'tool_chain.rs rejects empty chain',
  modSrc.includes('at least one step'),
  'guard against trivial compact_tool_chain({chain:[]})'
);
check(
  'tool_chain.rs rejects single-step chain',
  modSrc.includes('compact_tool_chain needs ≥ 2 steps'),
  'guard against routing 1-tool calls through the chain wrapper'
);
check(
  'tool_chain.rs caps chain at 32 steps',
  /maximum is 32/.test(modSrc),
  'DoS guard against LLM runaway chains'
);
check(
  'tool_chain.rs supports sequential mode',
  modSrc.includes('ChainMode::Sequential'),
  'sequential mode is the default for ordered pipelines'
);
check(
  'tool_chain.rs supports parallel mode via JoinSet',
  modSrc.includes('ChainMode::Parallel') && modSrc.includes('JoinSet'),
  'parallel mode uses tokio::JoinSet'
);
check(
  'tool_chain.rs handles abort_on_first_error',
  modSrc.includes('FailureMode::AbortOnFirstError'),
  'sequential mode aborts on first error by default'
);
check(
  'tool_chain.rs handles continue failure mode',
  modSrc.includes('FailureMode::Continue'),
  'parallel runs always continue even on errors'
);
check(
  'tool_chain.rs truncates per-step results at 4000 chars',
  modSrc.includes('per_step_max_chars: 4000'),
  'per-step content budget to bound LLM context growth'
);
check(
  'tool_chain.rs handles CJK strings at char boundary',
  /truncate_at_char_boundary_handles_cjk/.test(modSrc),
  'CJK-safe truncation (no byte-boundary panic)'
);
check(
  'tool_chain.rs omits None fields from JSON result',
  /skip_serializing_if = "Option::is_none"/.test(modSrc),
  'no `null` noise in tool_result JSON'
);

// 2. register_builtins wires CompactToolChainHandler.
const regPath = path.join(ROOT, 'crates/hermix-tools/src/register_builtins.rs');
const regSrc = fs.readFileSync(regPath, 'utf8');
check(
  'register_builtins registers compact_tool_chain',
  regSrc.includes('CompactToolChainHandler::new'),
  'tool must reach the LLM-visible toolset'
);
check(
  'register_builtins places compact_tool_chain in `compact` toolset',
  /"compact",\s*\n?\s*Arc::new\(crate::tools::tool_chain/.test(regSrc),
  'separable toolset so users can opt-out per-toggling'
);
check(
  'register_builtins uses registry.clone() not unsafe transmute',
  regSrc.includes('Arc::new(registry.clone())') &&
    !/unsafe[\s\S]{0,300}ToolRegistry[\s\S]{0,300}Arc<ToolRegistry>/.test(regSrc),
  'safe sharing via the Arc<Mutex<Inner>> inside ToolRegistry'
);

// 3. agent_loop.rs advertises compact_tool_chain in CONTEXT_BUDGET_GUIDANCE.
const agentPath = path.join(ROOT, 'crates/hermix-agent/src/agent_loop.rs');
const agentSrc = fs.readFileSync(agentPath, 'utf8');
check(
  'agent_loop.rs has CONTEXT_BUDGET_GUIDANCE const',
  agentSrc.includes('const CONTEXT_BUDGET_GUIDANCE: &str'),
  'system prompt documents the new RPC style'
);
check(
  'CONTEXT_BUDGET_GUIDANCE names compact_tool_chain',
  agentSrc.includes('compact_tool_chain'),
  'LLM must be told the tool name to call it'
);
check(
  'CONTEXT_BUDGET_GUIDANCE states "do NOT branch" caveat',
  /NOT to use it[\s\S]{0,500}branch/i.test(agentSrc),
  'warn LLM not to use chain for branching logic'
);
check(
  'agent_loop.rs conditionally inserts CONTEXT_BUDGET_GUIDANCE',
  /tool_names\.contains\("compact_tool_chain"\)[\s\S]{0,100}CONTEXT_BUDGET_GUIDANCE/.test(agentSrc),
  'guidance appears in system prompt only when tool is exposed'
);

// 4. tools/mod.rs exposes the module.
const modModPath = path.join(ROOT, 'crates/hermix-tools/src/tools/mod.rs');
const modModSrc = fs.readFileSync(modModPath, 'utf8');
check(
  'tools/mod.rs declares pub mod tool_chain',
  modModSrc.includes('pub mod tool_chain;'),
  'module is reachable from register_builtins'
);

console.log('');
const total = 19;
if (failed === 0) {
  console.log(`P0-2 smoke: all ${total} checks passed`);
  process.exit(0);
} else {
  console.error(`P0-2 smoke: ${failed} check(s) failed`);
  process.exit(1);
}
