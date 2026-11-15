---
name: evaluation
description: "Use when evaluating agent performance — running benchmarks, measuring task completion, or assessing output quality."
version: "1.0.0"
tags: [context-engineering, evaluation, benchmarks, testing]
---

## When to Activate

Activate this skill when:
- You need to measure agent accuracy on standard benchmarks
- You want to compare agent performance before/after changes
- You are setting up CI evaluation pipelines
- You need to assess task completion rates

Do **not** activate when:
- You need adversarial testing (use `advanced-evaluation` instead)
- You are just running unit tests (not agent evaluation)

## Core Concepts

### HermesBaseEnv Trait

Hermix evaluation is built on the `HermesBaseEnv` trait:
- `reset()`: Initialize a new episode
- `step(action)`: Execute an action and return observation
- `is_done()`: Check if the episode is complete
- `score()`: Calculate the episode score

### Benchmark Implementations

Three benchmark environments are available:
- **SWE-bench**: Software engineering task completion
- **Terminal-bench**: Terminal command execution accuracy
- **YC-bench**: Y Combinator-style startup task evaluation

### Trajectory Recording

Every evaluation run records a `Trajectory`:
- Per-step actions, observations, rewards
- Tool calls and their results
- Token usage and latency
- Final score and completion status

## Practical Guidance

1. Always record trajectories — they're essential for debugging
2. Run each benchmark at least 3 times for statistical significance
3. Compare against baseline (no skills loaded) to measure skill impact
4. Track metrics over time to detect regression

## Examples

```rust
let mut env = SweBenchEnv::new(task_id: "django-12345");
let mut traj = Trajectory::new();
let obs = env.reset().await?;
let result = env.step(action).await?;
traj.record(obs, action, result.reward);
```

## Gotchas

- Benchmarks require API access and token budget
- Some benchmarks need specific repository checkout
- Trajectory files can be large — archive old runs

## Integration

- `advanced-evaluation`: Adversarial testing extends basic evaluation
- `harness-engineering`: Skills are evaluated through these benchmarks
- `self-improvement-loops`: Evaluation results feed self-improvement

## References

- `crates/hermix-environments/src/training/` — HermesBaseEnv + benchmarks
- `crates/hermix-environments/src/training/benchmarks/swe_bench.rs`
- `crates/hermix-environments/src/training/benchmarks/terminal_bench.rs`
- `crates/hermix-environments/src/training/benchmarks/yc_bench.rs`
