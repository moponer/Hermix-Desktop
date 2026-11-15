---
name: context-compression
description: "Use when the context window is full or near capacity and you need to reduce token count while preserving essential information."
version: "1.0.0"
tags: [context-engineering, compression, summarization, token-management]
---

## When to Activate

Activate this skill when:
- Context pressure exceeds the configured threshold (typically 85%)
- The LLM is truncating or dropping important context
- You need to fit more information into a limited context window
- You are designing a compression strategy for long conversations

Do **not** activate when:
- You are just checking context size (use `context-optimization` instead)
- You are detecting quality loss after compression (use `context-degradation` instead)

## Core Concepts

### Five-Step Compression Pipeline

Hermix implements a five-step compression pipeline (2,146 lines):

1. **Redundancy Elimination**: Remove duplicate or near-duplicate messages
2. **Template Extraction**: Replace repetitive tool outputs with templates
3. **Semantic Summarization**: LLM-powered summarization of old messages
4. **Priority Reordering**: Keep high-priority messages, compress low-priority
5. **Budget Enforcement**: Hard-cap total tokens to the configured budget

### Compression Safety

- System + Persona layers are NEVER compressed
- Recent messages (last N turns) are preserved verbatim
- Compression results are logged and can be reviewed
- Circuit breaker prevents runaway compression costs

## Practical Guidance

1. Always set a compression target (e.g., reduce to 60% of current size)
2. Prefer redundancy elimination over summarization (cheaper, more reliable)
3. Use semantic summarization only for messages older than 10 turns
4. Monitor compression ratio — too aggressive compression causes degradation

## Examples

### Example: Triggering compression

```rust
let compressed = compression::summarize_messages_with_llm(
    &messages,
    &config,
    target_tokens,
).await?;
```

## Gotchas

- LLM-based summarization costs API tokens — use sparingly
- Compression is not free — it takes ~2-5 seconds per cycle
- Never compress the current turn's tool results — they're needed for the next step

## Integration

- `context-fundamentals`: Provides the context model being compressed
- `context-degradation`: Check quality after compression
- `context-optimization`: Set budgets and thresholds

## References

- `crates/hermix-agent/src/compression.rs` — Five-step compression pipeline
- `crates/hermix-agent/src/compression_health.rs` — Compression quality checks
- `crates/hermix-agent/src/compression_ops.rs` — Low-level compression operations
