#!/usr/bin/env node
// P2-8 smoke: verify the agentskills.io open-standard surface is wired.
// Static smoke — it pins the frontmatter parser extensions, the
// import_from_url action, the hub client, and the URL allowlist so a
// refactor can't silently regress cross-ecosystem skill interop.

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
let failed = 0;
function check(name, cond, hint) {
  const status = cond ? 'OK ' : 'FAIL';
  console.log(`[${status}] ${name}${hint ? '  — ' + hint : ''}`);
  if (!cond) failed++;
}

// 1. SkillFrontmatter struct extended with the standard fields.
const orch = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-agent/src/skill_orchestrator.rs'),
  'utf8'
);
check(
  'SkillFrontmatter has agentskills.io standard fields',
  /pub struct SkillFrontmatter[\s\S]+?pub version: Option<String>/.test(orch) &&
    /pub author: Option<String>/.test(orch) &&
    /pub license: Option<String>/.test(orch) &&
    /pub tags: Option<Vec<String>>/.test(orch) &&
    /pub allowed_tools: Option<Vec<String>>/.test(orch) &&
    /pub category: Option<String>/.test(orch),
  'optional fields for forward-compat ingest'
);

// 2. parse_yaml_frontmatter handles the new shapes.
check(
  'parser recognises block lists (multi-line `- item`)',
  /block_list_key|trimmed_for_list\.starts_with\("- "\)/.test(orch),
  'tags:\n  - a\n  - b shape'
);
check(
  'parser recognises inline lists `[a, b, c]`',
  /value\.starts_with\('\['\)/.test(orch),
  'compact list form'
);
check(
  'parser canonicalises kebab to snake (`allowed-tools` → `allowed_tools`)',
  /fn canonical_frontmatter_key/.test(orch) &&
    /key\.replace\('-', "_"\)/.test(orch),
  'agentskills.io spec uses kebab; we store snake'
);
check(
  'parser strips trailing ` # ...` comments',
  /line\.find\(" #"\)/.test(orch),
  'comment-aware scalar parsing'
);
check(
  'parser tolerates unknown fields (forward-compat)',
  /p2_8_unknown_fields_are_tolerated/.test(orch),
  "future agentskills.io fields won't break ingest"
);

// 3. import_from_url action wired in skill_manage handler.
const skills = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-tools/src/tools/skills.rs'),
  'utf8'
);
check(
  'skill_manage supports import_from_url action',
  /"import_from_url" =>/.test(skills),
  'new P2-8 action in SkillManageHandler'
);
check(
  'import_from_url enforces URL allowlist',
  /agentskills\.io\//.test(skills) &&
    /raw\.githubusercontent\.com\//.test(skills) &&
    /gist\.githubusercontent\.com\//.test(skills),
  'rejects arbitrary URLs to prevent exfiltration via side channel'
);
check(
  'import_from_url uses reqwest (no curl shell-out)',
  /reqwest::Client::builder/.test(skills) && /\.get\(url\)/.test(skills),
  'proxy/TLS/retry parity with the rest of the workspace'
);
check(
  'import_from_url dedupes by name (update vs create)',
  /existed = self\.provider\.get_skill\(&name\)/.test(skills) &&
    /update_skill.*create_skill/s.test(skills),
  'idempotent re-import path'
);
check(
  'import_from_url requires `name` frontmatter',
  /SKILL\.md is missing required `name` frontmatter/.test(skills),
  'rejects nameless payloads'
);

// 4. hermix-skills hub client still wires the agentskills.io API.
const hub = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-skills/src/hub.rs'),
  'utf8'
);
check(
  'hub client targets agentskills.io by default',
  /DEFAULT_HUB_URL: &str = "https:\/\/agentskills\.io\/api\/v1"/.test(hub),
  'canonical hub URL'
);
check(
  'hub client exposes search / download / upload',
  /pub async fn search_skills/.test(hub) &&
    /pub async fn download_skill/.test(hub) &&
    /pub async fn upload_skill/.test(hub),
  'full API surface for power users'
);

// 5. Unit tests pin the new frontmatter shapes.
const skillUtils = fs.readFileSync(
  path.join(ROOT, 'crates/hermix-tools/src/tools/skill_utils.rs'),
  'utf8'
);
check(
  'skill_utils uses serde_yaml for frontmatter parsing',
  /serde_yaml::from_str\(yaml_block\)/.test(skillUtils),
  'YAML-compliant parser'
);

console.log(failed === 0 ? '\nALL OK' : `\n${failed} FAILED`);
process.exit(failed === 0 ? 0 : 1);