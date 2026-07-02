#!/usr/bin/env node
// P0-1 smoke: verify the auto-skill learning loop surface is wired end-to-end.
// This is a *static* smoke — it doesn't spin a Rust runtime; it asserts the
// configuration knobs and tool schemas are present in source, so an LLM-driven
// session can't degrade the loop silently.

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..');
let failed = 0;
function check(name, cond, hint) {
  const status = cond ? 'OK ' : 'FAIL';
  console.log(`[${status}] ${name}${hint ? '  — ' + hint : ''}`);
  if (!cond) failed++;
}

// 1. auto_create action exists in skill_manage handler with quality gate.
const skillsPath = path.join(ROOT, 'crates/hermix-tools/src/tools/skills.rs');
const skillsSrc = fs.readFileSync(skillsPath, 'utf8');
check(
  'skills.rs has auto_create action',
  skillsSrc.includes('"auto_create" =>'),
  'skill_manage must expose auto_create for background-review extraction'
);
check(
  'skills.rs has auto_extract_from_conversation action',
  skillsSrc.includes('"auto_extract_from_conversation" =>'),
  'deterministic extraction path for P0-1 fail-safe'
);
check(
  'skills.rs enforces 3–80 char name',
  skillsSrc.includes('must be 3–80 chars'),
  'quality gate for auto_create'
);
check(
  'skills.rs rejects placeholder summary',
  skillsSrc.includes('placeholder'),
  'quality gate for auto_create'
);
check(
  'skills.rs rejects empty steps',
  skillsSrc.includes('at least one step'),
  'quality gate for auto_create'
);
check(
  'skills.rs writes YAML frontmatter',
  skillsSrc.includes('"---\\nname:'),
  'orchestrator must index skill cleanly'
);
check(
  'skills.rs dedupes via update when skill exists',
  skillsSrc.includes('auto-updated'),
  'no duplicate skills on re-extract'
);

// 2. agent_loop.rs wires auto_skill_extraction as a config field.
const agentPath = path.join(ROOT, 'crates/hermix-agent/src/agent_loop.rs');
const agentSrc = fs.readFileSync(agentPath, 'utf8');
check(
  'agent_loop.rs has auto_skill_extraction field',
  agentSrc.includes('pub auto_skill_extraction: bool'),
  'master off-switch wired into AgentConfig'
);
check(
  'agent_loop.rs gates background review on auto_skill_extraction',
  /if self\.config\.auto_skill_extraction[\s\S]{0,200}skill_creation_nudge_interval/.test(agentSrc),
  'background review must respect the off-switch'
);
check(
  'agent_loop.rs forces auto_skill_extraction off in child session',
  agentSrc.includes('cfg.auto_skill_extraction = false'),
  'child background-review session must not pollute user skills'
);
check(
  'SKILL_REVIEW_PROMPT prefers auto_extract_from_conversation',
  /auto_extract_from_conversation.*quality gate/s.test(agentSrc),
  'review prompt nudges LLM toward the gated extraction path'
);
check(
  'SKILLS_GUIDANCE documents P0-1',
  agentSrc.includes('P0-1 (auto-skill learning loop)'),
  'system prompt mentions the new behavior'
);

// 3. hermix-config exposes SkillsSettings.auto_extraction and AgentLoopBehaviorConfig.auto_skill_extraction.
const configPath = path.join(ROOT, 'crates/hermix-config/src/config.rs');
const configSrc = fs.readFileSync(configPath, 'utf8');
check(
  'SkillsSettings.auto_extraction field exists',
  /pub struct SkillsSettings[\s\S]{0,800}pub auto_extraction: bool/.test(configSrc),
  'user opt-out under skills.auto_extraction'
);
check(
  'AgentLoopBehaviorConfig.auto_skill_extraction field exists',
  /pub struct AgentLoopBehaviorConfig[\s\S]{0,1000}pub auto_skill_extraction: bool/.test(configSrc),
  'user opt-out under agent.auto_skill_extraction'
);
check(
  'default_skills_auto_extraction returns true',
  configSrc.includes('fn default_skills_auto_extraction() -> bool'),
);

// 4. agent_builder wires both config keys into AgentConfig.
const builderPath = path.join(ROOT, 'crates/hermix-agent/src/agent_builder.rs');
const builderSrc = fs.readFileSync(builderPath, 'utf8');
check(
  'agent_builder forwards auto_skill_extraction from config',
  /auto_skill_extraction: config\.agent\.auto_skill_extraction/.test(builderSrc),
  'config.yaml reaches AgentConfig'
);

// 5. Hermix user can opt out without disabling background_review.
const propPath = path.join(ROOT, 'crates/hermix-config/tests/prop_config_roundtrip.rs');
const propSrc = fs.readFileSync(propPath, 'utf8');
check(
  'config test pins skills.auto_extraction default to true',
  propSrc.includes('skills_auto_extraction_defaults_to_true'),
);
check(
  'config test pins agent.auto_skill_extraction default to true',
  propSrc.includes('agent_auto_skill_extraction_defaults_to_true'),
);
check(
  'config test verifies YAML opt-out round-trip',
  propSrc.includes('skills_auto_extraction_yaml_can_opt_out'),
);

console.log('');
if (failed === 0) {
  console.log(`P0-1 smoke: all ${12} checks passed`);
  process.exit(0);
} else {
  console.error(`P0-1 smoke: ${failed} check(s) failed`);
  process.exit(1);
}
