---
name: multi-agent-patterns
description: "Use when designing or implementing multi-agent coordination — orchestrator-worker, group chat, or pipeline patterns."
version: "1.0.0"
tags: [context-engineering, multi-agent, orchestration, groupchat]
---

## When to Activate

Activate this skill when:
- You need multiple agents to collaborate on a task
- You are designing an orchestrator-worker pattern
- You want to implement group chat with multiple personas
- You are debugging agent coordination issues

Do **not** activate when:
- You are working with a single agent (no coordination needed)
- You are just routing tools (use `tool-design` instead)

## Core Concepts

### Orchestrator Pattern

Hermix implements an orchestrator that:
- Delegates sub-tasks to specialized worker agents
- Manages context sharing via LatentBrief (compressed trajectory)
- Tracks worker progress and handles timeouts
- Merges worker results into the final response

### Group Chat

The groupchat module supports:
- Round-robin turn allocation
- Selector-based speaker selection
- Configurable termination conditions
- 10-file implementation covering all coordination patterns

### Sub-Agent Safety

Every sub-agent interaction is guarded:
- Sandboxed execution environment
- Limited tool access (whitelist per sub-agent)
- Output validation before merging
- Timeout enforcement

## Practical Guidance

1. Keep sub-agent tasks small and well-defined
2. Use LatentBrief to share context — don't send full history
3. Set appropriate timeouts for sub-agent tasks
4. Always validate sub-agent output before using it

## Examples

```rust
// Delegate a task to a worker
let brief = LatentBriefBuilder::new("Refactor module")
    .attention_keyword("agent_loop")
    .build();
let result = orchestrator.delegate(brief).await?;
```

## Gotchas

- Sub-agents share the same API rate limits — parallel calls can hit limits
- Group chat can loop infinitely without proper termination conditions
- Context sharing between agents is lossy — critical info may be lost

## Integration

- `latent-briefing`: Compressed context for sub-agents
- `hosted-agents`: Sandboxed execution environments
- `context-fundamentals`: Context management across agents

## References

- `crates/hermix-agent/src/orchestrator.rs` — Main orchestrator
- `crates/hermix-agent/src/group_chat/` — 10-file groupchat implementation
- `crates/hermix-agent/src/sub_agent_orchestrator.rs` — Worker delegation
- `crates/hermix-agent/src/sub_agent_safety.rs` — Safety guards
