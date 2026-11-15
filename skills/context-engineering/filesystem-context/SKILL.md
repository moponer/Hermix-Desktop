---
name: filesystem-context
description: "Use when managing file-based context — indexing project files, searching codebases, or providing file content to the LLM."
version: "1.0.0"
tags: [context-engineering, filesystem, indexing, search]
---

## When to Activate

Activate this skill when:
- You need to provide file content as context to the LLM
- You are building or querying a file index
- You want to search across project files efficiently
- You are managing which files are visible to the agent

Do **not** activate when:
- You are managing the context window itself (use `context-fundamentals` instead)
- You are compressing file content (use `context-compression` instead)

## Core Concepts

### File Context Manager

Hermix implements `context_files.rs` with:
- Automatic file discovery from project root
- SQLite + FTS5 full-text search index
- File type filtering (respect .gitignore)
- Incremental indexing (only re-index changed files)

### Context File Selection

When the LLM needs file context:
1. User query → keyword extraction
2. FTS5 search → candidate files ranked by relevance
3. Token budget check → select files that fit
4. Content injection → add selected files to working memory

## Practical Guidance

1. Keep the file index updated — stale indices return wrong results
2. Use .gitignore to exclude irrelevant files (node_modules, build artifacts)
3. Prefer FTS5 search over manual file listing
4. Always check token budget before injecting file content

## Examples

```rust
let files = context_files::search("authentication middleware", &index, budget: 2000).await?;
for file in files {
    context_manager.inject(file.content);
}
```

## Gotchas

- Large files can exhaust the token budget — use offset/limit
- Binary files are excluded from indexing but may be listed
- FTS5 uses Unicode-aware tokenization on newer SQLite versions

## Integration

- `context-fundamentals`: File content is part of the working memory layer
- `context-optimization`: File context has its own token budget
- `tool-design`: File search is exposed as a tool

## References

- `crates/hermix-agent/src/context_files.rs` — File context + SQLite FTS5
