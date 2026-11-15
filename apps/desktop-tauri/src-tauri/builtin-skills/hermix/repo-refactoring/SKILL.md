---
name: repo-refactoring
version: 1
description: >
  Analyze, refactor, and optimize an EXISTING codebase with a disciplined
  loop: orient → baseline → locate → smallest change → verify → report.
  Use when the task involves understanding an unfamiliar repo, changing or
  refactoring existing code, fixing a bug in a large project, optimizing a
  module, or "看一下这个仓库"、"重构这个模块"、"这段代码为什么慢"、"帮我优化
  这个项目的结构". Do NOT use for green-field code (nothing exists yet), pure
  Q&A about a codebase (read-only recon, use delegate_task), or document work.
license: MIT
tags:
  - code
  - refactoring
  - workflow
  - repository
---

# Skill: Repo Refactoring / Existing-Code Changes

Existing-code work is a **loop, not a leap**: orient → baseline → locate →
smallest change → verify → report. Every skipped phase converts engineering
into guesswork — and guesswork on someone's repo is how trust dies.

Pairs with `workflow-tdd` (when tests should be written first) and
`workflow-verification` (the honest-verification gate). This skill covers the
repo-level moves they assume.

## Phase 1 — Orient before touching

- Entry points first: README / AGENTS.md / workspace manifest
  (`Cargo.toml`, `package.json`, `pyproject.toml`), then the main module tree.
- Follow call chains, not file names: `search_files` for the symbol, then
  `read_file` the hits. **Never edit a symbol whose callers you haven't
  read** — the second caller you didn't see is where regressions are born.
- Extract the project's own check commands from its manifest, CI config, or
  scripts section (`cargo test`, `npm run build && npm test`, `pytest`).
  These are the acceptance oracle. If they don't exist, say so and propose
  adding them before making behavioral changes.
- Repo too large to hold? Delegate bounded recon via `delegate_task` with a
  written brief (paths to inspect, the question to answer, the acceptance
  command). Keep the conclusion, not the dump.

## Phase 2 — Baseline before change

- Run the relevant build/tests **before** editing anything:
  - green baseline → your changes are the only suspect when tests break;
  - red baseline → report it immediately, fix only what you own, never
    bundle an unrelated fix into your change without saying so.
- Note the baseline numbers (`47 passed, 2 skipped`) — you will compare
  against them in Phase 5.

## Phase 3 — Locate precisely, read enough

- The read set for any change is: the function you change, its direct
  callers, its tests, and the type/trait it implements or implements-for.
- Serial reads are a smell: batch independent `search_files` / `read_file`
  calls in one turn.
- Found the bug already? Still do Phase 2. "I know the fix" without a
  baseline is how a one-line fix ships with a broken build.

## Phase 4 — Smallest change that does the thing

- One concern per change. If you spot an unrelated bug or smell, note it in
  the final report — do not fix it silently inside this change.
- Match the surrounding style: naming, error handling, comment language.
  A refactor that re-styles the file buries the real diff.
- Prefer editing over rewriting. Keep public APIs stable unless the ask
  explicitly includes breaking changes.
- Refactoring = **behavior preservation**: if a test's assertions change,
  that is a behavior change and belongs in its own, declared step.

## Phase 5 — Verify with the project's own checks

- After each edit, re-run the Phase 2 commands. Report command + evidence:
  `cargo test -p hermix-cron: 47 passed`. Not "tests pass" — the command
  and the numbers.
- Cannot run them (missing toolchain, env)? Say `could not verify X
  (reason: …)` and stop claiming success. Never downgrade a failed run into
  a clean summary.
- New behavior without a test is unfinished work: add the test that would
  have caught the bug you just fixed.

## Phase 6 — Report like an auditor

- What changed (`file:line` list), what ran, what passed, baseline vs after.
- Unsolved problems, risks, and follow-ups at the end — once, not woven in.

## Anti-patterns

- Editing a symbol before reading its callers ("it looked self-contained").
- Discovering a red baseline AFTER your change and quietly fixing it inside
  the same diff.
- "Refactoring" that changes test assertions — that is a rewrite of intent.
- Reporting "done" with zero verification commands run.
- One giant diff mixing refactor + feature + style.
