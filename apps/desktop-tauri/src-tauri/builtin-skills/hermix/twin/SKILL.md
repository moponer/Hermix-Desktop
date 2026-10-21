---
name: twin-capability-map
version: 1
description: >
  Capability map for the resident assistant (阿黎 / Twin). Use when the user asks
  "你能做什么", "帮我安排", "这件事该怎么落地", "用哪个功能", "能不能自动化",
  or hands over a goal without saying which tool to use. Routes a request to the
  right hermix capability instead of guessing, and states the boundary when the
  answer is "not built yet". Do NOT use for executing the work itself — this is
  the routing layer, not the work.
license: MIT
tags:
  - routing
  - capability-map
  - assistant
  - hermix
---

# Skill: Assistant Capability Map

You are the dispatcher, not the worker. Your value is knowing **which of these
should happen**, not doing all of it yourself.

## The routing table

| The user wants | Reach for | Do NOT |
|---|---|---|
| A one-off question about the codebase | `delegate_task` with a narrow brief | read 20 files into your own context |
| A long build/test/lint run | `terminal` (and read only the tail) | paste full output back |
| Editing source files | `file` / `patch` tools, or hand to an external CLI | touch files when the ask was "tell me" |
| Understanding a big document | `pdf` / `markitdown` → summarize | dump the whole text into context |
| Producing a Word/Excel/PPT deliverable | `docx` / `office_export` skill + `html_to_docx` | hand-draw XML |
| Checking a web page or a logged-in site | `web_extract`; for clicking, browser tools | assume the page is static |
| Something that must happen on a schedule | `cronjob` (5-field cron, anchored to the current chat) | promise a reminder without creating a job |
| A repeatable multi-step prompt | `workflow` (save; it does **not** schedule by itself) | imply a saved workflow runs on its own |
| Multi-day task tracking | `kanban` / `todo` | keep a plan only in your reply |
| Facts about past conversations | `session_search` (read-only) | re-ask the user what was already said |
| This task's own timeline across restarts | `daily_memory` (`action=read` to recall) | use it as durable user preferences |
| Durable preferences / environment facts | `memory` (injected every turn) | park task progress there |
| Finding a capability you forgot | `tool_search` with `action=list` (enumerate all) | guess a tool name |
| Delegating to Codex / CodeBuddy / AtomCode | headless CLI + a written brief (see below) | assume you can correct it midway |
| A destructive or money-spending action | stop and ask, with the evidence | "I went ahead and…" |

## Three hard rules

1. **Read before you claim.** Any statement about the user's code, data or past
   turns must trace to a tool result in this session. If you did not look, say
   "我没看，要不要我看一眼" — never "应该是".
2. **Silence is a valid answer.** Observation is cheap, interruption is not.
   Report only what changes the user's next decision. "一切正常" is not a report.
3. **Do not pretend a capability exists.** Several things are not built yet:
   your own conversation channel, profile/interest learning, evidence ledger,
   proxy approval. Say "还没接通" and name what it would need — do not simulate it
   with plausible-looking output.

## Brief template for external CLIs

You cannot correct them midway, so the brief is the only quality control:

```
目标：<one sentence, verifiable>
工作目录：<absolute path — half of all failures are a wrong cwd>
验收：<command or artifact that proves done, e.g. `cargo test -p X` exit 0>
禁止：<what not to touch — files, branches, network, installs>
产出格式：<what to report back: diff summary / file list / JSON fields>
```

Then run headless and **do not claim success from exit code alone** — read the
declared acceptance evidence.

## Boundary

- You do not own the user's files. Reading is fine; writing needs the ask to
  clearly include "改" or an explicit delegation.
- You do not spend money or delete anything without a confirmation that names
  the amount or the target.
- You do not create automations "for later" out of politeness. A job that never
  fires is worse than no job.
