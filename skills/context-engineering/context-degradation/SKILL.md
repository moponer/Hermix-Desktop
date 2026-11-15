---
name: context-degradation
description: "Use when detecting or diagnosing context quality loss — hallucinations, repetition, instruction forgetting, or coherence drift during long conversations."
version: "1.0.0"
tags: [context-engineering, degradation, quality, diagnostics]
---

## When to Activate

Activate this skill when:
- The agent starts repeating itself or losing track of instructions
- You suspect context quality has degraded after many turns
- You need to diagnose why the agent is producing lower-quality responses
- You want to implement degradation detection in a new component

Do **not** activate when:
- You are compressing context (use `context-compression` instead)
- You are just checking context size (use `context-optimization` instead)

## Core Concepts

### Four-Level Degradation Diagnosis

Hermix implements `ContextHealth` with four diagnostic levels:

1. **Healthy**: All context layers intact, no quality loss
2. **Degraded**: Minor quality loss — some instructions may be partially forgotten
3. **Critical**: Significant quality loss — hallucinations likely, repetition detected
4. **Failed**: Context is unusable — session should be restarted or context reset

### Detection Signals

- **Repetition detection**: Same or similar content appearing in consecutive turns
- **Instruction forgetting**: Agent ignoring earlier system/persona instructions
- **Coherence drift**: Responses becoming tangentially related to the query
- **Error loop**: Consecutive tool failures suggesting context confusion

## Practical Guidance

1. Check `ContextHealth` after every compression cycle
2. When degraded, reduce context window usage by 20%
3. When critical, trigger aggressive compression or session reset
4. Log degradation events for self-improvement feedback

## Examples

### Example: Diagnosing context health

```rust
let health = context_engine::diagnose(&messages, &tool_results);
match health.level {
    ContextHealthLevel::Healthy => {},
    ContextHealthLevel::Degraded => reduce_context_by(0.20),
    ContextHealthLevel::Critical => trigger_aggressive_compression(),
    ContextHealthLevel::Failed => suggest_session_reset(),
}
```

## Gotchas

- Degradation can be subtle — don't wait for "Failed" to act
- Compression itself can cause degradation if important content is removed
- Some models are more resilient to degradation than others

## Integration

- `context-fundamentals`: Provides the context model being diagnosed
- `context-compression`: Compression can trigger degradation events
- `self-improvement-loops`: Degradation events feed into self-improvement

## References

- `crates/hermix-agent/src/context_engine.rs` — ContextHealth four-level diagnosis
- `crates/hermix-agent/src/convergence_detector.rs` — Error loop detection
