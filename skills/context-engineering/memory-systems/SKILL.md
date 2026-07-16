---
name: memory-systems
description: "Use when managing agent memory — storing, recalling, or organizing information across sessions and turns."
version: "1.0.0"
tags: [context-engineering, memory, persistence, recall]
---

## When to Activate

Activate this skill when:
- You need to store information for later recall across sessions
- You are implementing memory persistence (short-term or long-term)
- You need to recall relevant information from past interactions
- You are designing the memory architecture for a new agent feature

Do **not** activate when:
- You are just managing the context window (use `context-fundamentals` instead)
- You are compressing old messages (use `context-compression` instead)

## Core Concepts

### Five-Layer Memory Architecture

Hermix implements five memory layers via `hermix-focus/`:

1. **Echo**: Immediate echo of recent turns (rolling buffer)
2. **Buffer**: Short-term working memory (per-session)
3. **Focus Stack**: Priority-based attention management (FocusStack state machine)
4. **Salience Store**: Importance-weighted persistent memory (SQLite-backed)
5. **Time-indexed Recall**: Chronological retrieval with decay functions

### Focus Stack State Machine

The FocusStack manages what the agent is "paying attention to":
- **Push**: Add a new focus item (e.g., "debugging auth bug")
- **Pop**: Remove the top focus item
- **Replace**: Swap the top item without changing stack depth
- **Peek**: Read the top item without modifying the stack

### Vector Recall

Long-term memory uses fastembed for embedding:
- All stored memories are embedded at write time
- Recall uses cosine similarity to find relevant memories
- Results are re-ranked by recency and salience score

## Practical Guidance

1. Always embed memories at write time — embedding at read time is too slow
2. Use the Focus Stack for task-scoped attention; don't overload it
3. Set appropriate salience thresholds — too low and you recall noise
4. Use time-indexed recall for "what happened yesterday" queries

## Examples

```rust
// Store a memory with salience
memory.store("auth bug in login flow", salience: 0.8).await?;

// Recall relevant memories
let memories = memory.recall("authentication issue", limit: 5).await?;
```

## Gotchas

- Vector recall quality depends on the embedding model — BGE-small is good for English
- The Focus Stack has a max depth — overflowing silently drops the oldest item
- SQLite can be a bottleneck under heavy write load — batch writes when possible

## Integration

- `context-fundamentals`: Memory is a context layer
- `context-optimization`: Memory recall has its own token budget
- `bdi-mental-states`: Beliefs are stored in the memory layer

## References

- `crates/hermix-focus/` — Five-layer memory architecture
- `crates/hermix-agent/src/focus_stack_ops.rs` — FocusStack operations
- `crates/hermix-agent/src/recall_scheduler.rs` — Scheduled memory recall
- `crates/hermix-agent/src/memory_hooks.rs` — Memory event hooks
