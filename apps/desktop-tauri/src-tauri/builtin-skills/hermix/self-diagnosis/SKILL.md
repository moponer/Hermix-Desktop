---

name: self-diagnosis
version: 1
description: >
  Diagnose hermix itself when it misbehaves: replies stop midway, the agent
  returns nothing, a turn takes minutes, tools report "not found"/"not
  available", Office/PDF conversion fails, or the app hangs. Use when the user
  says "卡住了", "自己停了", "很慢", "工具用不了", "转换失败", "报错了", or asks
  why something in the app does not work. Produces a concrete verdict (which
  log line / which config knob) instead of guesses. Do NOT use for bugs in the
  user's own project code (that is ordinary debugging).
license: MIT
tags:
  - diagnosis
  - troubleshooting
  - logs
  - hermix
---

# Skill: Self-Diagnosis

Every hermix failure leaves a trace. Find the trace before proposing a fix.

## Where the evidence is

```
%USERPROFILE%\.hermix\logs\       (or $HERMIX_HOME/logs)
  agent.log    agent loop: turns, retries, convergence, compaction
  server.log   HTTP/WS: requests, approvals, disconnects
  tools.log    tool dispatch + timings
  crash.log    panics (EMPTY here ⇒ not a crash, the loop chose to stop)
```

Read them with `terminal` (`Select-String` on Windows / `grep` elsewhere).
Never ask the user to "send logs" before you looked yourself.

## Symptom → first grep

| Symptom | Grep for | Meaning |
|---|---|---|
| Reply stops by itself, no error | `hit hard timeout`, `no_first_chunk`, `stream_idle` | streaming watchdog fired; raise `agent.stream_*` or check the provider |
| Stops after several tool errors | `convergence detected` | `ErrorLoop` / `SimilarContent` hard stop |
| Long pause between turns | `turn_transition_stall` | gap between tool result and next turn (DB write? compaction?) |
| "Please approve" then nothing | `approval request timed out`, `channel_closed` | approval timeout / WS dropped |
| Slow but eventually answers | `LLM API retry in`, `429` | upstream rate-limit / retry backoff |
| Empty answer, model looks fine | `decide_empty_response`, `empty` | nudge budget exhausted |
| Tool "not found" / "unavailable" | tool name + `not registered` | tool gated by env (e.g. `HASS_URL`) or not in this toolset |
| Office/PDF conversion fails | `dsh 引擎`, `引擎按需下载失败`, `dsoffice` | conversion engine not yet downloaded — first use fetches ~71MB into `<hermix home>/dsh-engine`; a failure there is network or sha512 (per-mirror detail is in the log line) |

## Configuration knobs that matter

`~/.hermix/config.yaml`:

```yaml
agent:
  stream_first_chunk_timeout_secs: 30   # raise for slow reasoning providers
  stream_overall_timeout_secs: 180
  stream_idle_timeout_secs: 120         # post-first-chunk silence → partial reply
  # approval_timeout_secs: 600         # shrink if approvals hang turns
```

Changing a key that no code reads is not a fix — verify the field is bridged
(`agent_builder.rs` `build_agent_config`).

## Report format

```
现象: <one line, with timestamps>
证据: <log file>:<line> — <exact matched text>
判定: <which mechanism fired / which component is missing>
修法: <config change | code location | command to run>
未验证: <what you could not check>
```

If the logs show nothing, say so explicitly and list which log files you read —
"no evidence" is a finding, not a failure.

## Anti-patterns

- Restarting the app first (destroys the in-memory evidence you need).
- Quoting a symptom back to the user as the diagnosis.
- Suggesting a config key without confirming it is read.
- Blaming the model provider before checking `agent.log` for a local stop.
- Deleting logs "to clean up" — they are the only forensic record.
