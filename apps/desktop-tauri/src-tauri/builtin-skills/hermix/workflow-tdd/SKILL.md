---

name: workflow-tdd
version: 1
description: >
  Test-first discipline for coding work in this repo. Use when implementing a
  feature, fixing a bug that has a reproducible trigger, or changing behaviour
  that is already covered by tests. Forces a failing test (or an explicit,
  written reproduction) BEFORE touching production code, keeps each red→green
  cycle small, and refuses "I'll add tests later". Also use when the user says
  "TDD", "test first", "write a failing test", "regression test", or asks why a
  change is safe. Do NOT use for pure exploration, docs-only edits, or
  throwaway scripts.
license: MIT
tags:
  - tdd
  - testing
  - regression
  - workflow
---

# Workflow: Test-Driven Development

The point of TDD is not coverage. It is that **you are not allowed to claim a
behaviour exists until something that can fail says it does**.

## The loop (small steps, no skipping)

1. **State the behaviour in one sentence.** "`patch` rejects an edit when the
   file changed since the last read." If you cannot write the sentence, you do
   not yet know what to build.
2. **Write the failing check first.**
   - Prefer a Rust unit test next to the code (`#[cfg(test)] mod tests`).
   - If the code path is only reachable through the binary/sidecar, write the
     reproduction as a shell/HTTP call and record the exact expected output.
   - Run it and **confirm it fails for the right reason** (not a compile error
     in the test itself, not a missing import). Paste the failure into your
     reasoning before continuing.
3. **Make it pass with the least code.** No speculation about future needs —
   that is what `ponytail` is for.
4. **Refactor with the test green.** Move code between files, rename, split
   modules; rerun after each move.
5. **Commit-sized increments.** One behaviour per cycle.

## In this repo

- Fast inner loop: `cargo test -p <crate> --lib -- <name_filter>` — never run
  the whole workspace to check one test.
- Compile-only check: `cargo check -p <crate> --all-targets`.
- Golden/parity fixtures exist for the agent loop. If you change observable
  loop behaviour, update the fixture **in the same change** and say so.
- Bug fixes: the regression test must fail on the pre-fix code. If it passes
  before your fix, you fixed something else.

## Anti-patterns (each one is a refused request)

- "I'll add tests after it works." → Then you don't know it works.
- Writing the test to match the implementation you just wrote. → That tests
  nothing. Derive expectations from the requirement.
- Asserting on log text instead of returned values/state unless the log line IS
  the contract.
- One giant test covering five behaviours: when it fails you learn nothing.
- Marking a test `#[ignore]` to get green. Fix it or delete it with a note.

## Exit criteria

- Failing-first evidence exists for every new behaviour.
- `cargo test -p <crate> --lib` is green.
- You can name the test that would catch a regression of this change.
