---
name: advanced-evaluation
description: "Use when running adversarial benchmarks, regression testing, or sophisticated evaluation scenarios beyond basic task completion."
version: "1.0.0"
tags: [context-engineering, evaluation, adversarial, regression, benchmarks]
---

## When to Activate

Activate this skill when:
- You need adversarial testing (trying to break the agent)
- You are running regression tests to catch performance degradation
- You need per-skill accuracy metrics and confusion matrices
- You are implementing the benchmark suite (Stage 0-3)

Do **not** activate when:
- You need basic task completion metrics (use `evaluation` instead)
- You are just running unit tests

## Core Concepts

### Benchmark Suite Stages

Hermix implements a four-stage benchmark suite:

- **Stage 0**: Skill health scoring (deterministic, no LLM calls)
- **Stage 1**: Activation case regression (tool routing accuracy)
- **Stage 2**: Router accuracy (multi-model top-1/top-3/MRR)
- **Stage 3**: Effectiveness (with/without skills comparison)

### Adversarial Scenarios

Seven adversarial scenarios test robustness:
1. Duplicate mechanism injection
2. Unretrieved source citation
3. Incorrect rubric calculation
4. Self-approving rubric modification
5. Weak-evidence novelty claims
6. Aggregate score optimization with critical dimension failure
7. Missing failure result recording

### Report Aggregation

Each stage produces a typed report with `aggregate()`:
- `SkillHealthReport`: Corpus-wide health statistics
- `RouterAccuracyReport`: Per-model top-1/top-3/MRR
- `EffectivenessReport`: With/without skill comparison
- `AdversarialReport`: Pass/fail per adversarial scenario

## Practical Guidance

1. Run Stage 0 first (free, deterministic) — fix any flagged skills
2. Stage 1 requires no API calls — run in CI
3. Stage 2+ require API budget — run on schedule, not every PR
4. Always save reports to `docs/` for historical tracking

## Examples

```rust
let report = benchmark_suite::run_skill_health(&skills_dir);
assert!(report.avg_composite >= 0.85);
```

## Gotchas

- Adversarial scenarios may expose real bugs — don't ignore failures
- MRR scores are sensitive to the number of candidate skills
- Effectiveness benchmarks require identical tasks with/without skills

## Integration

- `evaluation`: Base evaluation infrastructure
- `harness-engineering`: Skills are the units being evaluated
- `tool-design`: Router accuracy is a key benchmark metric

## References

- `crates/hermix-agent/src/benchmark_suite.rs` — P6 benchmark data structures
- `crates/hermix-agent/src/skill_health.rs` — Skill health scoring
