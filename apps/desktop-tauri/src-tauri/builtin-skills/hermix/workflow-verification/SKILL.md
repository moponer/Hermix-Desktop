---

name: workflow-verification
version: 1
description: >
  Evidence-based completion: do not claim "done" until something outside your
  own reasoning proves it. Use before finishing any task that changed code,
  config, data, or files — especially before saying "fixed", "done",
  "implemented", "all tests pass", or "should work now". Turns vague claims into
  rerunnable evidence (command output, test names, file diffs, screenshots) and
  states explicitly what was NOT verified. Also use when the user asks "is it
  really done", "did you test it", or when a previous "fixed" turned out to be
  wrong. Do NOT use to pad trivial answers with ceremony.
license: MIT
tags:
  - verification
  - evidence
  - completion
  - workflow
---

# Workflow: Verification Before Completion

Plans, checklists, and confident prose are **not** evidence. A conclusion is
earned by an artifact someone else could re-run.

## Evidence ladder (weak → strong)

1. "The code looks right." — worth nothing.
2. "It compiles." — proves syntax.
3. "The unit test that encodes the requirement passes." — good.
4. "The failing reproduction now passes, and its regression test is in the
   suite." — strong.
5. "End to end: real command / real file / real HTTP call produced the expected
   output, shown below." — strongest.

Match the level to the claim: a UI claim needs a rendering/screenshot or a DOM
probe; a performance claim needs numbers; a "no regressions" claim needs the
full suite result for the affected crates, not one green test.

## Checklist before saying "done"

- [ ] The exact command(s) you ran are listed, with their result lines.
- [ ] For each requirement in the request: which check proves it? (If a
      requirement has no check, either add one or say "not verified".)
- [ ] Warnings: `cargo check -p <crate> --all-targets` shows none new.
- [ ] Tests: `cargo test -p <crate> --lib` green; name the new/changed tests.
- [ ] Known limits stated: env not available, platform not tested, path not
      exercised, feature flag off.
- [ ] Anything you changed "to be safe" is listed as an unverified change.

## Honesty rules

- Never say "all tests pass" if you ran a filtered subset — say the filter.
- Never say "fixed" for something you only compiled.
- If a failure is environmental (missing permission, no runtime), say so with
  the exact error and what would need to change — do not quietly skip it.
- If you stopped early (context/time), say what remains, with the next command
  to run. A precise "not done, here's where" beats a vague "done".

## In this repo

- Sidecar freshness: `./scripts/check-sidecar-freshness.sh` before packaging
  claims. Stale sidecars have caused real delivery incidents.
- Config changes: the value must be reachable from `~/.hermix/config.yaml` — a
  serde field nobody bridges is not a config feature.
- Tool behaviour: a schema/description change is part of the API; check the
  bridge (`bridge_tool_registry`) exposes it.

## Output shape

```
改了什么: <one line per change>
验证: <command> → <result line>
未验证: <explicit list or "none">
剩余风险: <one line or "none">
```
