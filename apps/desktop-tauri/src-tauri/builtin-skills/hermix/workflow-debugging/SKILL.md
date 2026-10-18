---

name: workflow-debugging
version: 1
description: >
  Systematic debugging: reproduce, localise, explain, then fix — in that order.
  Use when something "doesn't work", a test fails unexpectedly, the app hangs or
  stops silently, an output is wrong, or the user reports a bug without a stack
  trace. Prevents the two classic wastes: changing code hoping it helps, and
  fixing the symptom while the cause survives. Also use when the user says
  "debug", "why is this failing", "it stopped", "flaky", or "works on my
  machine". Do NOT use for greenfield design questions.
license: MIT
tags:
  - debugging
  - diagnosis
  - logs
  - workflow
---

# Workflow: Systematic Debugging

Rule zero: **you may not edit production code until you can state the cause in
one sentence that predicts the symptom.**

## Phases

### 1. Reproduce (or bound it)

- Get the smallest command/input that shows the failure. If it is intermittent,
  record frequency and the conditions you can control (time, load, provider).
- Write the reproduction down (command + expected vs actual). This becomes the
  regression test later.

### 2. Localise

- Read the actual error text. Do not paraphrase it away — the difference between
  `timed_out` and `channel_closed` is usually the whole diagnosis.
- Binary-search the path: which layer sees the symptom first? Add a cheap probe
  (log line, `dbg!`, HTTP echo) at the boundary, not everywhere.
- For this repo: `~/.hermix/logs/{agent,server,tools,crash}.log` are the first
  stop. Grep for the concrete markers (`hit hard timeout`, `no_first_chunk`,
  `convergence detected`, `approval request timed out`, `LLM API retry in`)
  before guessing. An empty `crash.log` rules out panics.
- Check the input side before the logic side: wrong model name, empty env var,
  missing file, stale sidecar binary (see `check-sidecar-freshness.sh`).

### 3. Explain

Write: "X happens because Y at <file:line>, which means Z." If you cannot fill
in the line number, you are still in phase 2.

### 4. Fix the cause, then the class

- Fix the specific cause.
- Ask: what else shares this cause? (Same unwrap, same timeout constant, same
  missing validation.) Fix the class or file a note — do not silently leave
  siblings broken.
- Add the regression test from phase 1 BEFORE the fix when possible.

### 5. Verify

- The reproduction now passes.
- `cargo test -p <crate> --lib` green.
- No new warnings: `cargo check -p <crate> --all-targets`.

## Anti-patterns

- Changing several things then re-testing: if it works you learned nothing.
- Adding a `sleep`/`retry` to hide a race — say "this masks a race" out loud and
  get agreement.
- Deleting a failing test to get green.
- Treating a timeout as a logic bug until you have seen the timing numbers.
- Assuming the user's description is the mechanism ("it froze" often means a
  silent early return, not a hang).
