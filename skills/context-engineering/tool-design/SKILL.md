---
name: tool-design
description: "Use when designing, routing, or managing tools and their schemas for LLM agent interactions."
version: "1.0.0"
tags: [context-engineering, tools, routing, schemas]
---

## When to Activate

Activate this skill when:
- You are adding a new tool to the agent's tool suite
- You need to route a user request to the appropriate tool
- You are designing tool schemas for LLM consumption
- You want to optimize tool selection accuracy

Do **not** activate when:
- You are implementing a skill document (use `harness-engineering` instead)
- You are managing agent orchestration (use `multi-agent-patterns` instead)

## Core Concepts

### Tool Schema Router

Hermix implements a 1,096-line `ToolSchemaRouter` that:
- Matches user intent to the best tool using schema analysis
- Ranks tools by relevance score (semantic + keyword matching)
- Supports top-1, top-3, and MRR evaluation metrics
- Handles ambiguous queries with disambiguation prompts

### Tool Registration

Tools are registered via the `inventory` crate for compile-time auto-registration:
- Each tool declares its name, description, and parameter schema
- The router builds an in-memory index at startup
- Schema changes require recompilation

### Tool Validation

Before execution, tools are validated:
- Parameter types checked against schema
- Required parameters verified as present
- Dangerous operations flagged for user confirmation

## Practical Guidance

1. Keep tool descriptions concise but specific — they're the router's primary signal
2. Include `Use when` phrases in tool descriptions for better routing
3. Test routing accuracy with the benchmark suite before deploying new tools
4. Avoid overlapping tool descriptions — they confuse the router

## Examples

```rust
let router = ToolSchemaRouter::new(&tool_registry);
let results = router.route("search for files containing TODO").await?;
assert!(results[0].tool_name.contains("search"));
```

## Gotchas

- The router uses both semantic and keyword matching — don't optimize for just one
- Adding too many tools degrades routing accuracy (signal dilution)
- Tool descriptions should avoid jargon — the LLM needs to understand them

## Integration

- `context-fundamentals`: Tools operate within the context layer
- `project-development`: Tool design is part of project architecture
- `evaluation`: Tool routing accuracy is a benchmark target

## References

- `crates/hermix-intelligence/src/tool_router.rs` — 1,096-line tool router
- `crates/hermix-agent/src/tool_dispatch.rs` — Tool dispatch logic
- `crates/hermix-agent/src/tool_call_validator.rs` — Parameter validation
