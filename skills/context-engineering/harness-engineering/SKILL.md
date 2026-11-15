---
name: harness-engineering
description: "Use when designing, implementing, or managing skill harnesses — the controlled environments where skills are loaded, executed, and evaluated."
version: "1.0.0"
tags: [context-engineering, harness, skills, safety, permissions]
---

## When to Activate

Activate this skill when:
- You are creating or modifying a skill document (SKILL.md)
- You need to understand skill loading, validation, and execution
- You are implementing skill safety or permission controls
- You want to add a new skill to the corpus

Do **not** activate when:
- You are just using skills (not implementing them)
- You are evaluating skill quality (use `evaluation` instead)

## Core Concepts

### Skill Lifecycle

1. **Author**: Write SKILL.md with frontmatter + structured body
2. **Validate**: SkillGuard checks security, SkillValidator checks structure
3. **Load**: SkillOrchestrator scans and indexes the skill
4. **Route**: ToolSchemaRouter matches user intent to skill
5. **Execute**: Agent follows skill instructions in context
6. **Evaluate**: Benchmark suite measures skill effectiveness
7. **Improve**: Self-improvement loop proposes edits

### Four-Layer Permission Model

Hermix implements four layers of safety:
1. **Content guard**: Blocks dangerous patterns (rm -rf, sudo, etc.)
2. **URL guard**: Blocks internal/local network URLs
3. **Tool whitelist**: Skills can only use allowed tools
4. **Execution sandbox**: Optional isolation for tool execution

### Skill Document Structure

Every SKILL.md must include:
- YAML frontmatter: `name`, `description` (with trigger phrase)
- `## When to Activate`: When to use (and when NOT to)
- `## Integration`: How it relates to other skills

Recommended sections:
- `## Core Concepts`, `## Examples`, `## Gotchas`, `## References`

## Practical Guidance

1. Always include trigger phrases in descriptions (`Use when ...`)
2. Keep SKILL.md under 500 lines — longer docs get truncated
3. Include at least one code example in a fenced block
4. Reference specific `.rs` files in `## References`
5. Test with `score_skill_md()` — aim for ≥ 0.85 composite

## Examples

```markdown
---
name: my-skill
description: "Use when you need to XYZ"
---
## When to Activate
When XYZ is needed.

## Core Concepts
...

## Integration
Works with `other-skill`.
```

## Gotchas

- Skills without trigger phrases will never be routed to
- Missing `## When to Activate` causes validation warnings
- Very long skills (>500 lines) get soft-truncated, losing content

## Integration

- `evaluation`: Skills are evaluated through benchmarks
- `advanced-evaluation`: Adversarial tests target skill quality
- `self-improvement-loops`: Skills can be auto-improved

## References

- `crates/hermix-agent/src/skill_orchestrator.rs` — Skill scanning and indexing
- `crates/hermix-agent/src/skill_loader.rs` — Skill loading
- `crates/hermix-agent/src/skill_health.rs` — Health scoring
- `crates/hermix-skills/src/guard.rs` — Security validation
- `crates/hermix-skills/src/validator.rs` — Structure validation
