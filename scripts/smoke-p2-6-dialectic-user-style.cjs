#!/usr/bin/env node
// P2-6 smoke: verify the dialectic user-style detector is wired
// across the stack. Static smoke — it asserts the rule-based
// detector, the fact-row writer, the system-prompt guidance, and
// the test coverage are all in place. Honcho's full dialectic
// profile is out of scope; this pins the *minimum useful* slice:
// detect style per turn → write `kind=user.style:*` fact → teach
// the LLM to adapt.

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
let failed = 0;
function check(name, cond, hint) {
  const status = cond ? 'OK ' : 'FAIL';
  console.log(`[${status}] ${name}${hint ? '  — ' + hint : ''}`);
  if (!cond) failed++;
}

// 1. user_style module exists in hermix-focus with the detector API.
const focusLib = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-focus/src/lib.rs'),
  'utf8'
);
check(
  'user_style module exported from hermix-focus',
  /pub mod user_style/.test(focusLib),
  'public API surface'
);

const userStyle = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-focus/src/user_style.rs'),
  'utf8'
);
check(
  'detect_user_style function exposed',
  /pub fn detect_user_style\(user_turn: &str\) -> Option<UserStyle>/.test(userStyle),
  'single-turn detector (no LLM round-trip)'
);
check(
  'UserStyle struct carries style + confidence + evidence',
  /pub struct UserStyle[\s\S]+?pub style: String[\s\S]+?pub confidence: f64[\s\S]+?pub evidence: Value/.test(userStyle),
  'explainable detection so the LLM can audit why it was tagged'
);
check(
  'detector classifies technical / direct / exploratory / mixed',
  /"technical"/.test(userStyle) &&
    /"direct"/.test(userStyle) &&
    /"exploratory"/.test(userStyle) &&
    /"mixed"/.test(userStyle),
  'four-way classification covering the Honcho-comparable axes'
);
check(
  'detector enforces 0.4 confidence threshold (no noisy facts)',
  /0\.4/.test(userStyle),
  'suppress low-signal writes'
);
check(
  'unit tests cover empty / technical / direct / exploratory / mixed',
  /fn empty_returns_none/.test(userStyle) &&
    /fn detects_technical_via_code_fence/.test(userStyle) &&
    /fn detects_direct_short_imperative_no_politeness/.test(userStyle) &&
    /fn detects_exploratory_long_with_hedges/.test(userStyle) &&
    /fn tie_falls_through_to_mixed/.test(userStyle),
  '6 unit tests pinning every branch'
);

// 2. agent_loop writes the style fact per turn via the embedded backend.
const al = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-agent/src/agent_loop.rs'),
  'utf8'
);
check(
  'agent_loop calls detect_user_style per turn',
  /hermix_focus::user_style::detect_user_style/.test(al),
  'dialectic detector wired into the per-turn loop'
);
check(
  'agent_loop writes the style fact via upsert_fact',
  /user\.style:\{\}/.test(al) || /user\.style:\$\{/.test(al),
  'canonical_key = user.style:<style> (UNIQUE-deduped)'
);
check(
  'style fact tagged as dialectic',
  /"dialectic"/.test(al),
  'distinguishes dialectic tags from user-asserted facts'
);

// 3. agent_loop wires USER_STYLE_GUIDANCE into the system prompt.
check(
  'USER_STYLE_GUIDANCE constant defined',
  /const USER_STYLE_GUIDANCE: &str = "P2-6 \(dialectic user style\)/.test(al),
  'system prompt block'
);
check(
  'USER_STYLE_GUIDANCE tells LLM how to adapt per style',
  /`technical` → skip/.test(al) &&
    /`direct` → no preamble/.test(al) &&
    /`exploratory`/.test(al) &&
    /`mixed`/.test(al),
  'per-style adaptation rules'
);
check(
  'USER_STYLE_GUIDANCE injected into tool_guidance',
  /tool_guidance\.push\(USER_STYLE_GUIDANCE\)/.test(al),
  'always-on regardless of which tools are present'
);

// 4. facts table already supports UNIQUE-key dedup so the
//    `user.style:<style>` row converges across sessions.
const embedded = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-focus/src/embedded.rs'),
  'utf8'
);
check(
  'facts table has UNIQUE INDEX on canonical_key (visibility=1)',
  /facts_canonical_key_uidx[\s\S]{0,200}ON facts\(canonical_key\) WHERE visibility = 1/.test(embedded),
  'writes dedup so style tag converges across sessions'
);

console.log(failed === 0 ? '\nALL OK' : `\n${failed} FAILED`);
process.exit(failed === 0 ? 0 : 1);