---
name: project-development
description: "Use when planning, designing, or building a software project from requirements through deployment. Covers task-model fit analysis, staged pipeline architecture, structured output design, cost estimation, and minimal architecture principles."
version: "1.0.0"
tags: [context-engineering, methodology, architecture]
---

## When to Activate

Activate this skill when:
- You are planning a new software project or major feature
- You need to decide between different architectural approaches
- You are designing a data processing pipeline
- You need to estimate costs and choose models for an LLM-powered system
- You are evaluating whether a task is better suited for prompting, fine-tuning, or traditional code

Do **not** activate when:
- You are debugging an existing system (use `debugging` instead)
- You are writing tests for known behavior (use `testing` instead)
- You are refactoring without changing architecture (use `refactoring` instead)

## Core Concepts

### Task-Model Fit Analysis

Not every problem needs an LLM. Use this decision framework:

| Task Type | Best Approach | Example |
|-----------|--------------|---------|
| Deterministic transform | Traditional code | CSV parsing, date formatting |
| Pattern matching with rules | Regex / rule engine | Email validation, log parsing |
| Semi-structured extraction | LLM with structured output | Invoice parsing, entity extraction |
| Creative / open-ended generation | LLM with few-shot | Copywriting, code generation |
| Multi-step reasoning | LLM with chain-of-thought | Math, planning, analysis |

### Staged Pipeline Architecture

Most LLM-powered systems follow a 5-stage pipeline:

1. **Acquire**: Gather raw input (files, APIs, user input)
2. **Prepare**: Clean, chunk, and format for the model
3. **Process**: Run through the LLM with appropriate prompting
4. **Parse**: Extract structured output from the response
5. **Render**: Present results to the user or downstream system

Each stage should be independently testable and replaceable.

### Structured Output Design

When using LLMs in production:
- Always define a JSON schema for expected output
- Use `response_format: { type: "json_object" }` when available
- Validate output against the schema before processing
- Have a fallback for malformed responses (retry with error message)

### Cost Estimation

Formula for estimating LLM costs:
```
cost_per_call = (input_tokens × input_price + output_tokens × output_price) / 1M
monthly_cost = cost_per_call × calls_per_day × 30
```

Key heuristics:
- Prompt engineering is 10-100x cheaper than fine-tuning
- Caching reduces costs by 50-90% for repeated queries
- Smaller models are 5-50x cheaper and sufficient for most tasks

### Minimal Architecture Principle

Start with the simplest architecture that could work:
1. Single prompt → single response
2. Add structured output
3. Add retry + validation
4. Add caching
5. Add multi-step pipeline (only if needed)

Never skip to step 5 without proving steps 1-4 insufficient.

## Practical Guidance

### Project Planning Checklist

- [ ] Define success criteria (measurable outcomes)
- [ ] Choose task-model fit (prompting vs fine-tuning vs code)
- [ ] Design the 5-stage pipeline
- [ ] Define structured output schemas
- [ ] Estimate costs (tokens × calls × price)
- [ ] Plan for failures (retry, fallback, validation)
- [ ] Set up evaluation metrics (accuracy, latency, cost)

### Cost Optimization Patterns

1. **Prompt caching**: Cache system prompts and few-shot examples
2. **Model routing**: Use cheap models for easy queries, expensive for hard
3. **Batch processing**: Group similar requests for throughput
4. **Output length limits**: Set `max_tokens` to avoid runaway costs

## Examples

### Example: Building a Document Q&A System

**Task-model fit**: Semi-structured extraction + creative generation → LLM with RAG

**Pipeline design**:
```
Acquire: User question + relevant document chunks (via vector search)
Prepare: Format as prompt with context window management
Process: LLM generates answer grounded in provided context
Parse: Extract answer and source citations
Render: Display answer with clickable references
```

**Cost estimate** (1000 queries/day):
- Input: ~2000 tokens × $0.15/M = $0.0003/query
- Output: ~500 tokens × $0.60/M = $0.0003/query
- Daily: $0.60, Monthly: ~$18

## Gotchas

- **Premature optimization**: Don't add complexity until you've measured the baseline. A single prompt often works better than expected.
- **Over-engineering the pipeline**: Each stage adds latency and failure points. Merge stages when possible.
- **Ignoring structured output**: Without a schema, you'll spend more time parsing than generating.
- **Cost surprises**: Always estimate token usage before deployment. Long contexts can be 10x more expensive than expected.
- **Model selection bias**: Bigger isn't always better. For classification tasks, a fine-tuned small model often outperforms a prompted large model.

## Integration

- **context-fundamentals**: Understand context windows before designing pipelines
- **context-compression**: Apply compression when input exceeds context limits
- **context-optimization**: Use budget management to control costs
- **tool-design**: Design tools that integrate with your pipeline stages
- **evaluation**: Set up evaluation metrics from day one

## References

- Hermix architecture: `docs/ARCHITECTURE_REFACTOR_PLAN.md`
- Hermix optimization plan: `docs/OPTIMIZATION_PLAN.md`
- Hermix context model: `crates/hermix-agent/src/context.rs`
- Hermix cost guard: `crates/hermix-agent/src/cost_guard.rs`
- Hermix model routing: `crates/hermix-agent/src/smart_model_routing.rs`
