---
name: context-optimization
description: "Use when managing context budgets, cost constraints, or optimizing token allocation across context layers."
version: "1.0.0"
tags: [context-engineering, optimization, budget, cost]
---

## When to Activate

Activate this skill when:
- You need to allocate token budget across context layers
- You are implementing cost controls for LLM API usage
- You want to optimize which context to keep vs. evict
- You are configuring context pressure thresholds

Do **not** activate when:
- You are compressing context (use `context-compression` instead)
- You are detecting quality loss (use `context-degradation` instead)

## Core Concepts

### Budget Allocation

Hermix allocates context budget across layers:
- System + Persona: Fixed, non-negotiable (~20-30%)
- Working Memory: Dynamic, compressed when over budget
- Long-term Memory: On-demand retrieval, limited by recall budget
- Skill Layer: Loaded per-turn based on routing

### Cost Guard

The `cost_guard` module prevents runaway API spending:
- Per-session token budget
- Per-request cost caps
- Daily/monthly spending limits
- Automatic model downgrading when budget is low

### Context Pressure

`context_pressure.rs` calculates real-time context utilization:
- utilization = current_tokens / max_tokens
- When utilization > threshold → trigger compression
- When utilization > 95% → emergency truncation

## Practical Guidance

1. Set realistic budgets based on your API plan
2. Monitor cost_guard metrics after each session
3. Use smart model routing to avoid spending premium tokens on simple tasks
4. Profile your typical context usage before setting thresholds

## Examples

```rust
let pressure = context_pressure(&messages, &config);
if pressure.utilization > 0.85 {
    compress_messages(&messages, pressure.suggested_target);
}
```

## Gotchas

- Budget includes ALL tokens (input + output), not just input
- Compression costs tokens too — budget for compression overhead
- Cost limits are per-session, not per-task

## Integration

- `context-fundamentals`: Layer model being optimized
- `context-compression`: Triggered by pressure thresholds
- `tool-design`: Smart routing reduces unnecessary token spend

## References

- `crates/hermix-agent/src/context_pressure.rs` — Pressure calculation
- `crates/hermix-agent/src/budget.rs` — Budget allocation
- `crates/hermix-agent/src/cost_guard.rs` — Cost control
- `crates/hermix-agent/src/smart_model_routing.rs` — Model routing
