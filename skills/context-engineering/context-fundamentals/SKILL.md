---
name: context-fundamentals
description: "Use when managing context windows, understanding context layers, or optimizing what information is available to the LLM at each turn."
version: "1.0.0"
tags: [context-engineering, context-window, fundamentals]
---

## When to Activate

Activate this skill when:
- You need to manage what information enters the LLM context window
- You are deciding which context layers to include or exclude
- You are debugging context-related issues (missing info, hallucinations)
- You need to understand how context is structured in Hermix

Do **not** activate when:
- You are compressing context (use `context-compression` instead)
- You are detecting context degradation (use `context-degradation` instead)
- You are managing context budget (use `context-optimization` instead)

## Core Concepts

### Five-Layer Context Model

Hermix implements a five-layer context architecture:

1. **System Layer**: Immutable system prompt + safety instructions
2. **Persona Layer**: Personality, soul.md, behavioral directives
3. **Working Memory**: Current task context, recent tool results
4. **Long-term Memory**: Persistent facts, user preferences (SQLite-backed)
5. **Skill Layer**: Active skill documents and routing tables

Each layer has a priority that determines what gets evicted first when the context window fills up.

### Context Window Management

- System + Persona layers are never evicted
- Working memory is managed by the compression pipeline
- Long-term memory is retrieved via vector similarity (fastembed)
- Skill layer is loaded on-demand based on tool routing

## Practical Guidance

1. Always check `ContextManager::layer_sizes()` before adding content
2. Use `context_pressure()` to detect when the window is nearing capacity
3. Prefer vector recall over stuffing everything into working memory
4. Keep soul.md concise — it occupies permanent context budget

## Examples

### Example: Checking context pressure

```rust
let pressure = context_pressure(&messages, &config);
if pressure.utilization > 0.85 {
    // Trigger compression before adding more content
    let compressed = compress_messages(&messages, target_budget);
}
```

## Gotchas

- The five layers are NOT equally sized — System + Persona typically consume 20-30% of the window
- Vector recall can return irrelevant results if the embedding model doesn't match the query language
- soul.md changes require a session restart to take effect

## Integration

- `context-compression`: Called when pressure exceeds threshold
- `context-degradation`: Monitors for quality loss after compression
- `context-optimization`: Manages budget allocation across layers
- `memory-systems`: Provides the long-term memory layer

## References

- `crates/hermix-agent/src/context.rs` — 5-layer context model
- `crates/hermix-agent/src/context_pressure.rs` — pressure calculation
- `crates/hermix-agent/src/prompt_assembly.rs` — layer assembly logic
