//! System-prompt direction collection.
//!
//! Extracted from `agent_loop.rs` (P2.x, ) so the 10k-line
//! `agent_loop.rs` can focus on the `impl AgentLoop` block. **No behavior
//! change** — pure relocation.
//!
//! Items living here:
//! - 15 guidance-block string constants (formatting / thinking / tool usage /
//!   plan / autonomous / memory / skills / context budget /
//!   user style / tool enforcement / OpenAI/Google/MiniMax operational hints).
//! - `collect_directions` — assemble the route-gated [`SystemDir`] list that
//!   the [`SystemPromptBuilder`] renders into the system prompt.

// ---------------------------------------------------------------------------
// Guidance blocks (route-gated system-prompt injections).
// ---------------------------------------------------------------------------

const MEMORY_GUIDANCE: &str = "You have persistent memory across sessions. Save durable facts using the memory tool: user preferences, environment details, tool quirks, and stable conventions. Memory is injected into every turn, so keep it compact and focused on facts that will still matter later. Prioritize what reduces future user steering.\n\n## What TO remember\n- User preferences and work style\n- Environment details (OS, tools, paths)\n- Tool quirks and stable conventions\n- Project-specific rules that reduce future steering\n\n## What NOT to remember (exclusion list)\n- Code patterns, project architecture, file structure (grep/git/AGENTS.md can retrieve these)\n- Git history and recent changes (git log/blame is the authority)\n- Debugging solutions and fixes (already in the code)\n- Content already in AGENTS.md or project context files\n- Temporary task state and current conversation context\n- One-off commands or session-specific actions\n\nCore principle: information derivable from current code should NEVER be stored. Code is 'live', memory is 'static' — storing it freezes it and may become 'authoritatively wrong'.";

const SESSION_SEARCH_GUIDANCE: &str = "When the user references something from a past conversation or you suspect relevant cross-session context exists, use session_search to recall it before asking them to repeat themselves.";

const SKILLS_GUIDANCE: &str = "You have persistent skills across sessions. Use `skill_manage` to save, update, or patch reusable workflows.\n\n## Actions\n- `auto_extract_from_conversation` — **preferred for saving**: enforces a quality gate (name 3–80 chars, summary >10 chars, ≥1 step), writes SKILL.md frontmatter, and dedupes by name.\n- `create` / `update` — manual skill management.\n- `patch` — append an 'Improvement Feedback' section to an existing skill (don't rewrite the whole file).\n\n## Quality gate (enforced)\n- `name`: kebab-case slug, 3–80 chars\n- `summary`: one paragraph, >10 chars, non-placeholder\n- `steps`: ordered array of concrete actions (≥1)\n- `rationale` (optional): why this works\n\n## Notes\n- When you find an existing skill outdated or incomplete, patch it immediately.\n- If nothing genuinely reusable emerged, do nothing.";

const CONTEXT_BUDGET_GUIDANCE: &str = "`compact_tool_chain` collapses N tool_results into one LLM-context entry — the main lever for keeping long sessions under the context budget.\n\n## Schema\n`{chain: [{tool, params, label?}, ...], mode?: 'sequential'|'parallel', failure_mode?: 'abort_on_first_error'|'continue', intent?: string}`\n\n## Constraints\n- Chain must be 2–32 steps.\n- Mode defaults to `sequential`; failure_mode defaults to `abort_on_first_error`.\n- Per-step results are truncated to 4000 chars each — use the tool directly (no chain wrapper) if you need the full output.\n- Chain steps don't feed into each other — results are collected, not piped.";

const TOOL_USAGE_GUIDANCE: &str = r#"
# Tool selection discipline

Before calling a generic tool like `terminal` or `execute_code`, **scan the available tool list** for a purpose-built alternative. Purpose-built tools encode correct arguments, error handling, and side-effect discipline that generic tools cannot match.

**Per-tool routing rules live in each tool's own description** — trust them (document conversion → `markitdown`; images by path → `read_image` inside a sub-agent; text/code → `read_file`; document authoring → the loaded `document-authoring` skill's `<hermix-canvas>` pipeline; rename/move → `move_file`). The system prompt carries only the cross-cutting rule below.

## Delegate heavy reads to sub-agents

Everything you read into the MAIN context is paid for **on every subsequent turn for the rest of the session**. As raw file/image/PDF dumps pile up, the context fills with noise and you **drift off the user's actual request** — echoing file contents, repeating searches, going in circles. This is the #1 cause of a session that loses the plot. Sub-agents are isolated: they read + analyze, then their context is destroyed, so the MAIN agent never pays the token cost.

**Default: prefer `delegate_task` for any file read.** The sub-agent returns only a concise conclusion.

**MUST go through `delegate_task` (do NOT call directly in the MAIN context) for:**
- **Images** — `read_image` injects a 2,000–8,000 token base64 blob per image, paid forever. Delegate to a sub-agent that calls `read_image`.
- **Large files** — anything > ~50 KB / ~1500 lines, or any PDF **> 5 pages** (a 100-page PDF is 30K+ tokens).
- **Reads you don't branch on** — if you only need a summary/answer, let the sub-agent read + analyze and return the conclusion.

**You still SEE these tools** (e.g. `read_image`, `read_pdf`) in your tool list — they are real tools, just meant to run *inside* a sub-agent. Their descriptions say so. When a task needs one, call `delegate_task` and let the sub-agent invoke it.

- **Inline images**: if the user **pasted/attached** an image in the current message, it is already visible via multimodal context — analyze it directly, no tool needed.
- `delegate_task` is **SYNCHRONOUS** — it blocks until the sub-agent returns the result as a normal tool result. No polling.
- For 4+ images: split into batches of 3-5 per `delegate_task` call, run in parallel.
- Example: `delegate_task(task="Summarize this PDF and list key clauses", context="Path: C:\\docs\\contract.pdf")`
- NEVER inline file contents into the `task` parameter — pass paths, let the sub-agent read.
- NEVER issue `terminal` / `execute_command` with unbounded output (`cat` on a large file, `dir /s /b`, logs without grep) — delegate to a sub-agent that paginates + filters.

This applies to ALL data types — PDFs, Word docs, Excel, codebases, search results, logs, web pages, API responses, DB exports. Sub-agent isolation is the general mechanism; only the image *attachment* format is special.

## Forbidden shapes in `terminal` arguments

The Tirith pre-check engine **blocks** tool calls whose arguments match any of the following shapes:

- **Windows raw path namespace** — never write `\\?\D:\...`, `\\?\UNC\server\...`, or `\\.\COM1` in a `terminal` `command`. Use the normal form (`D:\data`, `\\server\share\file`).
- **System directories** — do not `cat`, `type`, `dir`, `find`, `ls`, or otherwise shell-read `C:\Windows\System32`, `C:\Windows\SysWOW64`, `C:\Windows\WinSxS`, or `C:\Windows\assembly`. Use the `read_file` tool against the user-supplied workspace path instead.
- **Shell-injection shapes** — do not begin a `command` with a flag-like prefix immediately followed by a metacharacter (e.g. `-n && rm`, `-e | sh`).
- **Filesystem-root operations** — `find / -delete`, `rm /*`, `chmod -R 777 /` and similar require explicit user approval.
"#;

const PLAN_BEFORE_EXECUTE_GUIDANCE: &str = "\
# Planning format

When you plan, keep it to one or two natural-language sentences in the reasoning flow: state the concrete outcome, the next step, and what will confirm success. Do not emit a standalone `Plan` section or a generic numbered checklist in the final answer.

**Announcing a step is not doing it.** The moment you name a next action, that action belongs in a tool call *in this same turn*. Ending the turn on a plan — '接下来我会…', '1. … 2. … 3. …', 'Let me…' — is the most common way this loop stalls: the user gets a plan and has to type \"继续\". If a tool fails, state the new route in one clause, then call it.";

const EXPLORE_EFFICIENCY_GUIDANCE: &str = "\
# Exploration efficiency

- Issue independent reads/searches in the SAME turn in parallel — not queued one per turn.
- Don't re-read a file you already read, or list the same directory twice — the information hasn't changed.
- Pass commands inline rather than writing temp script files (`.ps1`/`.sh`/`.py`) into the project. If a file is genuinely unavoidable, it belongs in the system temp dir and must be deleted once the run succeeds — see \"Where files you create belong\".";

const EXPLORE_EFFICIENCY_SHORT: &str = "\
## Exploration efficiency
Issue independent reads in parallel. Don't re-read files or re-list directories. Don't write temp scripts.";

const PARALLEL_EXECUTION_GUIDANCE: &str = "\
# Parallel tool execution
When you have two or more tool calls whose arguments do NOT depend on each other's results, issue them in the same turn in parallel rather than queuing them one per turn. Only serialize when a later call genuinely needs an earlier result (e.g. 'use the path the previous call returned').";

const TOOL_USE_ENFORCEMENT_GUIDANCE: &str = "# Tool-use enforcement\nYou MUST use your tools to take action. Do not describe what you would do without actually doing it. When you say you will perform an action, make the corresponding tool call in the same response. Every response should either (a) contain tool calls that make progress, or (b) deliver a final result.";

/// Where files may be written. Injected into every agent's system
/// prompt (J1 of the 2026-09-30 plan).
///
/// The failure this prevents is concrete: during development sessions
/// the agent drops probe scripts, route dumps and plan drafts into the
/// repo root or `docs/` top level, where a later session reads them as
/// project assets. The three layers below (prompt / tool guard / CI
/// gate) all point at these same rules — see
/// `docs/notes/proposed/process/2026-09-30-doc-cleanup-and-optimization-plan.md`
/// §批次 J and the AGENTS.md 文档制度节.
const FILE_WRITE_DISCIPLINE: &str = "\
## Where files may be written (File Write Discipline)

Writing to the wrong place is a defect, not a style preference — a stray
plan or probe file reads as project truth to the next session.

1. **Source tree** — only modify files that already exist, and new source
   files must land inside an existing module's structure. A new crate or
   top-level directory is an architecture decision: write a decision
   note under `docs/notes/proposed/{class}/` and stop there.
2. **Plans, designs, analysis** → `docs/notes/proposed/{class}/yyyy-mm-dd-topic.md`
   (classes: feature / bug-fix / simplification / architecture / process /
   testing). Never a new `.md` in `docs/` top level, never in the repo root.
3. **Throwaway scripts, probes, debug dumps** → do not write them to disk at
   all. Run them with `code_execution` in memory. If a file is genuinely
   needed, put it under `%TEMP%\\hermix-scratch\\` (see the temp-file
   hygiene section) and say so.
4. **Logs, intermediate output** → never in the repo root. Tool output
   already lands in your context; a file adds nothing.
5. **User's own files** — never delete, move, or overwrite anything you did
   not create in this session.";

/// Injected when `AgentConfig::autonomous_mode = true`.
/// Streamlined : the original 8 rules collapsed into a single
/// stopping-condition principle + execution bias.  The nudge is
/// one-shot only (see empty_response_handler; the multi-round nudge
/// loop was removed 2026-07-09).
/// Reference: prompt-comparison-report.md § 四·十一, openclaw Execution Bias.
const AUTONOMOUS_EXECUTION_GUIDANCE: &str = "\
# Autonomous execution

You are in autonomous mode. Your only valid stopping condition is **task \
completion**. Do not stop mid-task unless no available tool can make further \
progress — and in that case, clearly explain what you tried and what blocks you.

## Completion requires evidence
A task is complete only when you have produced a **verified outcome**:
- You changed or confirmed something: file written, command succeeded, test \
  passed, question answered with concrete data, investigation concluded with \
  actionable findings.
- Actions, not intentions. \"Here's a plan\", \"I could try X\", or a directory \
  listing without follow-up is not completion — execute first, then report.

## During execution
- Tools before questions. Look it up, try it, or vary the approach before \
  asking the user.
- If a tool fails, switch tools or arguments — don't repeat what already failed.
- Non-final turns: advance with tool calls, not promises.";

/// Safety guardrail — always-on, independent of autonomous mode.
/// Injected unconditionally. Does not replace the tool-level approval engine;
/// it serve as a prompt-level backstop for models that ignore tool-side
/// guards (especially fine-tunes and non-OpenAI providers).
/// Reference: prompt-comparison-report.md § 九, openclaw Safety block.
const SAFETY_GUIDANCE: &str = "\
# Safety

- No self-preservation, replication, or resource acquisition beyond the task \
  assigned by the user.
- Safety and oversight take priority over completion. When in conflict: \
  pause and explain the concern — do not proceed.
- Do not persuade users to expand access, disable safeguards, or take \
  actions that could cause harm.";

/// System-prompt block injected for MiniMax / MiniMax-style models.
/// ## Why this exists
/// MiniMax-M2, MiniMax / Hermes fine-tunes, and several other
/// tool-call-friendly open-weight models expect tool calls to be
/// emitted as raw XML wrapped in `<minimax:tool_call>…</minimax:tool_call>`
/// (the model's native format) rather than through the OpenAI `tools`
/// API field. If we don't tell the model to use a recognised shape, it
/// either:
/// (a) emits the tool call as unstyled text the user can see, then
/// stalls while the agent loop retries the unparseable response,
/// or
/// (b) falls back to its own internal "tool format" that the
/// `HermesToolCallParser` doesn't recognise, leaving the
/// tool call unexecuted.
/// This block is appended after the standard tool-use guidance for
/// models that match `minimax`, `m2`, or `hermes-3` in
/// `should_inject_tool_enforcement` (see below). It points the model at
/// the **standard** Hermes XML format (`<function_calls><invoke …>`)
/// which `HermesToolCallParser` already understands, and explicitly
/// warns against emitting the `<minimax:tool_call>` wrapper as bare
/// text — the agent loop has a streaming-side redactor that strips
/// leaked wrappers, but the cleaner path is to not produce them.
const MINIMAX_TOOL_FORMAT_GUIDANCE: &str = "# MiniMax / MiniMax-M2 tool-call format\n\
The OpenAI `tools` API field is fully supported for you — emit tool calls through it\n\
(the same way you would for any OpenAI-compatible API). Do NOT wrap your tool calls\n\
in `<minimax:tool_call>…</minimax:tool_call>` blocks, and do NOT print raw XML tool\n\
calls into the visible reply. If a previous turn showed you doing this, that was\n\
a parse failure on the agent's side; the agent already strips leaked XML, but\n\
emitting structured tool calls through the API is the only way the agent will\n\
actually execute the call. If for any reason the structured path is unavailable,\n\
fall back to the standard Hermes XML form:\n\n\
```\n\
<function_calls>\n\
<invoke name=\"TOOL_NAME\">\n\
<parameter name=\"PARAM\">VALUE</parameter>\n\
</invoke>\n\
</function_calls>\n\
```\n\n\
## CRITICAL — never emit empty or placeholder tool calls\n\
If you do NOT need to call a tool for the current turn, **output only text — do NOT\n\
emit a tool call at all**. The agent loop interprets `finish_reason: \"tool_calls\"`\n\
with an empty / unfilled tool-call as a dead end: no tool executes, no text reaches\n\
the user, and the turn is wasted. When in doubt, answer the user directly instead of\n\
issuing a speculative tool call you cannot fill with real arguments.";

/// 「报告必须分节」单一真相源 —— 曾在 THINKING_DISCIPLINE_GUIDANCE 与
/// format.short 各有一份逐字拷贝（两处必须同步改），现收敛到这一处，
/// 两个注入点在运行时拼接。
const REPORT_SECTIONING: &str = "## 报告必须分节(钉死,无法绕过)\n\
凡是包含多步骤操作、改动多个文件、跨工具组合、阅读/解释代码、错误排查、调研对比等**实质报告**,不论字数多少,**一律用 # / ## / ### Markdown 标题分节**,严禁堆一整段纯文本。\n\
- **长报告(>300 字 或 >3 个步骤)** — 必须至少 1 个 ## 二级标题 + 至少 2 个 ### 三级标题分节(例如:`## 改动概览` → `## 关键文件` → `## 验证步骤`)\n\
- **多步任务(任何超过 2 步的操作)** — 用有序列表(`1. ` `2. ` `3. `)+ ### 二级标题收尾\n\
- **对比 / 选项 / 优缺点** — 一律表格(`| ... | ... |`)分列,不准用\"优点:... 缺点:...\"长句堆\n\
- **极简单行回复**(< 60 字,如一次性命令确认、单个数字回答)可直接说人话,但**只要涉及\"做了什么、改了什么、解释了什么\" → 必须分节**\n\
例外仅限:用户明确问\"一句话回我\"\"只要结论\"——否则一律分节。";

const THINKING_DISCIPLINE_GUIDANCE: &str = "# Output format\n\
Structure every reply as two parts:\n\
1. `<think>...</think>` — internal reasoning (private, user sees it as a collapsible section).\n\
2. Your reply, starting on the next line after `</think>`.\n\n\
**Always close `</think>` before any visible text.** Forgetting this breaks the renderer — the user sees nothing. If you have nothing to reason about, emit `<think></think>`. Use only `<think>...</think>`, never `<thinking>`.\n\n\
Write your visible reply in clean, structured Markdown: use `##`/`###` headings to separate topics, `-`/`1.` lists for steps and enumerations, `**bold**` for key terms, and fenced code blocks (```lang) for commands or code. Substantive answers (multi-step, comparisons, causes/conclusions, or anything beyond a one-liner) must be organized this way so the reader can scan it fast — do not dump a wall of plain prose.\n\n";

/// Safety instruction: do not generate or guess URLs.
const URL_SAFETY_GUIDANCE: &str = "\
# URL safety (mandatory)\n\
You must NEVER generate or guess URLs, links, or web addresses. \
If the user explicitly provided a URL in their message or a local file path, use it as-is. \
Do not construct URLs from memory, do not guess domain names, and do not \
suggest links to external resources unless the user directly provided them.\n\n\
The exception: file paths and URLs returned by tools (web_search, web_extract) \
are verified and safe to use.";

/// 网页与自动化能力地图。模型只能从工具表推断自己的能力，缺了这张地图
/// 时出现过两类误答（2026-10-16）：① 用户要访问/登录网站，模型答
/// 「我没有浏览器能力」；② 用户要「创建自动化」，模型做了个自定义应用。
const WEB_AUTOMATION_GUIDANCE: &str = "\
# 网页与自动化能力地图
- `url_safety` — 只做链接安全策略检查，不访问页面。
- `web_search` / `web_extract` — 搜索 + 抓取**公开**网页正文（无登录态、无 JS 交互）。
- `browser_navigate / browser_snapshot / browser_click / browser_type / browser_screenshot / browser_close` — 完整浏览器自动化（无头虚拟窗口，用户可在应用内小窗旁观）：打开页面、点按钮、填表单、**登录网站**、截图。用户要访问/登录/操作网站时用这条线，不要只让用户手动截图。
- `cronjob` — 定时任务/自动化（「每天自动…」「定时提醒…」）。用户要自动化时用 cronjob，不要为此创建自定义应用。
- `workflow` — 把跑通的流程存成带参数的可重跑工作流（用户在「自动化 → 工作流」页面一键重跑）。用户要「保存/复用这套流程」时用 workflow；跨会话的经验沉淀才用 skill。
- **验证码**：`browser_screenshot` 截图后用 `read_image` 亲眼看图（你有原生视觉）——普通字符码读出填入并让用户核对；滑块/点选类请用户看小窗口头报码。不要声称"视觉识别失败"就放弃，也不要尝试绕过验证码。
若你的工具列表里看不到任何 browser_* 工具，说明浏览器工具集未开启——如实告知用户去 设置 → 工具集 开启 browser，不要声称「我没有浏览器能力」。";

/// Inform the model that context compression may occur.
const COMPRESSION_AWARENESS_GUIDANCE: &str = "\
# Context compression\n\
The system may automatically compress earlier parts of the conversation \
when the context approaches capacity limits. This is normal and does not \
mean anything was lost permanently. If you notice a summary section \
titled \"[CONTEXT COMPACTION]\" in the conversation history, that is the \
system's compressed version of earlier turns. Read it to stay aware of \
what was discussed and decided, but do not repeat work already marked as complete.";

const OPENAI_MODEL_EXECUTION_GUIDANCE: &str = "# Execution discipline (OpenAI)\nUse tools whenever they improve correctness, completeness, or grounding. Do not stop early when another tool call would materially improve the result. Verify outcomes before declaring completion.";

const GOOGLE_MODEL_OPERATIONAL_GUIDANCE: &str = "# Operational guidance (Google)\nBe concise and execution-first. Prefer absolute paths, parallel tool calls when safe, and verify each substantive change.";

const TOOL_FAILURE_GUIDANCE: &str = "\
# Tool failure rule (mandatory, always-on)

1. **Never claim success on a failed tool call.** If a tool returns an error or \
failure status, you MUST report the actual failure state to the user — do NOT say \
'已删除' / 'done' / 'completed' / '成功' as if it succeeded. State the real outcome \
(e.g. '工具调用失败：No entry matched') first, then explain what it means.
2. **Summarize from the tool's actual result, not your intent.** If the call failed, \
the operation did not happen. Do not describe what you *tried* to do as if it *did* \
happen.
3. **Do NOT retry the same call twice.** If a tool fails twice, switch approach or \
ask the user.
4. **A single tool failure is NOT a reason to stop.** Analyze the error, switch tools \
or fix arguments, and continue making progress. Only stop when the task is complete \
or you have genuinely exhausted every reasonable approach — and then clearly tell the \
user what failed and what blocked you.
5. **You MUST end with a visible summary — never with an empty reply.** Before you \
decide to stop (finish_reason=\"stop\"), you MUST emit a natural-language summary in \
the visible channel (NOT inside a <think> block): what you did, what succeeded, what \
failed, and — if the task is incomplete — what the user should do next. A turn that \
stops with zero visible text is a failure regardless of how much tool work happened. \
If your draft was all reasoning, move the conclusion into the visible reply before \
stopping.

# Approval cards (important)\
Approval cards are sent AUTOMATICALLY by the server when a high-risk tool call (e.g. `terminal`) is about to execute — you do NOT trigger them. There is no tool that approves a request; calling `approval_token` or any other tool will NOT show an approval card. If you want a command to run without a popup, the user can pre-authorize it via `approval_token` (grant action), but that is separate from the interactive approval card. Never try to invoke an approval card through a tool call.";

/// Environment awareness + Shell compatibility + tool fallback priority.
/// Always-on: tells the model what OS/shell it runs on (Windows + unified
/// PowerShell shell layer), how to call PowerShell cmdlets correctly, and
/// that shell failures should trigger a switch to structured tools — not
/// more shell retries.
const ENVIRONMENT_SHELL_GUIDANCE: &str = "\
# Environment & Shell (Windows)

You are running on **Windows**. The `terminal` tool runs commands through a unified PowerShell shell layer — a single routing, commands passed through as argv (no cmd.exe string joining). Built-in POSIX aliases `ls`→Get-ChildItem and `cat`→Get-Content work; `grep` has no built-in alias — use `Select-String` for searching.

## Shell compatibility facts
- Prefer native PowerShell cmdlets over cmd.exe one-liners: `Get-ChildItem` over `dir`, `Get-Content` over `type`, `Select-String` over `findstr`, `Get-Process` over `tasklist`. POSIX aliases work too but cmdlets are more capable.
- **Path format**: use native backslash form (`C:\\Users\\x\\file.txt`). PowerShell also accepts forward slashes, but backslash is the native form.
- **Quote handling**: quote paths with spaces; for inline scripts with `$` variables, use `-EncodedCommand <base64>` to avoid interpretation issues.
- **Environment variables**: PowerShell-native `$env:TEMP`, `$env:PATH`, `$HOME` work. In bash-style `$x` form, use `powershell -EncodedCommand` for anything with `$` signs.

## Structured tools bypass shell issues
- `storage_analyzer` (Safe, read-only) — disk usage scanning.
- `execute_code` (Python, read-only) — `os.scandir`, `os.stat`, `pathlib` pass `CodeExecutionGuard` (Approved, no confirmation). Deletion via Python is blocked; use `storage_clean`.
- `disk_cleanup` (Caution, dry_run defaults true) — prune stale files under HERMIX_HOME.
- `storage_clean` (Dangerous, requires approval) — delete/move-to-recycle specific paths.
- `move_file` — native OS filesystem API for copy/move/rename. `copy: true` to copy, `overwrite: true` to replace. No shell involved.

## Read-only operations never need approval
Directory listing, file size scanning, disk space queries, and process/service listings are read-only. Structured tools marked Safe never prompt. `terminal` read-only PowerShell commands (`Get-ChildItem`, `Get-PSDrive`, `Measure-Object`) should not trigger confirmation.";

/// Where a file you create is allowed to live, and when you must delete it.
///
/// 起因（2026-10-02）：模型把一次性脚本 `add.py` 写进了
/// `apps/desktop-tauri/src-tauri/`（进程 cwd = 项目源码树），污染仓库。
/// 旧规则只有 EXPLORE_EFFICIENCY 里一句「别把临时脚本写进项目」——
/// 只劝阻、没给替代位置，模型无处可去就退回 cwd。
///
/// 核心区分（用户原话：「有些文件本身就是要留存磁盘的，那么应该删除的
/// 是 LLM 自己根据任务需要写的，写完自己去跑的，跑完以后无用了就顺手
/// 清理掉」）：
///
///   **产物 (deliverable)** — 任务要交付的东西。报告、生成的数据文件、
///     用户要的脚本、导出物。**留在磁盘上，路径要告诉用户。**
///
///   **脚手架 (scaffolding)** — 你为了完成���务而写的中间物。跑一次就
///     完成的验证脚本、试算草稿、格式转换的临时文件。**跑完即删。**
///
/// 判据不是「我是不是用 write_file 写的」，而是「**用户会不会在任务
/// 结束后还想要它**」。
const FILE_LIFECYCLE_GUIDANCE: &str = "\
# Where files you create belong (mandatory)

## The line: deliverable vs scaffolding

Before writing any file, ask: **after this task ends, will the user still want this file?**

- **YES → deliverable (产物).** Reports, generated data, exports, scripts the user asked for.
  Keep it on disk. Always state the full path in your reply so the user can find it.
- **NO → scaffolding (脚手架).** A verification script you wrote to run once, a scratch
  calculation, an intermediate conversion file. **Write it, run it, then delete it in the
  same task.** Do not leave it behind \"in case it's useful later\" — the user did not ask for it.

The test is not \"did I write it with write_file\". The test is intent: a one-shot script the
user never mentioned is scaffolding, even though you spent three turns debugging it.

## Scaffolding must go to the system temp directory

Your process cwd is the **project source tree**. Writing there pollutes the repository:
it shows up in `git status`, breaks builds, and is the user's problem to clean up.

- **Prefer not writing a file at all.** Inline the logic into a `terminal` / `execute_code`
  command whenever it fits in one line or a short here-string. This is the best option.
- **When a file is genuinely needed** (multi-line script, or a file the interpreter must
  read from disk), write it under the system temp directory — `$env:TEMP` on PowerShell,
  `%TEMP%` otherwise. Never under the project directory, never under `src/`.
- **Delete scaffolding when the task completes.** Use `storage_clean` (it moves to Recycle
  Bin, so it is recoverable) rather than a raw `Remove-Item`. Mention the cleanup in one
  short clause so the user knows nothing was left behind.

## Never write into these without an explicit user request

- The project source tree (`src/`, `crates/`, `apps/`) — that is the user's code, not your scratch space.
- `node_modules/`, `target/`, `dist/`, `.git/` — build/VCS internals; a file there is invisible to the user and survives cleanup.

## If a tool call fails on permissions

Do **not** respond by relocating the file to a different directory and retrying. A
permission error (`SetNamedSecurityInfoW failed`, access denied) means the *location* is
wrong, not that you should hunt for a location that happens to be writable. Move to the
temp directory and say why in one line.";

const GIT_SAFETY_GUIDANCE: &str = "\
# Git safety protocol (mandatory)

When working with Git, follow these rules strictly:

## Never do
- Never modify git config
- Never execute destructive commands: push --force, reset --hard, checkout ., clean -f
- Never skip hooks (--no-verify)
- Never force push to main/master
- Never amend commits that were already pushed

## Always do
- Always create NEW commits rather than amending existing ones
- If a commit fails (e.g. pre-commit hook rejected), fix the issue and create a NEW commit — do NOT use --amend
- Verify the commit message before pushing
- Check git status before committing to ensure only intended files are included

## Why this matters
Using --amend after a failed commit can modify an unrelated previous commit, causing code loss. Always create new commits to preserve history.";

const SKILL_USAGE_GUIDANCE: &str = "\
# Skills

## How to use a Skill
1. Check `<available_skills>` for a matching Skill.
2. Load the Skill with `skill_view(name='skill-name')`.
3. Follow the Skill instructions step by step.";

const TODO_GUIDANCE: &str = "\
# Task list (`todo` tool)

The `todo` tool lets you track a multi-step plan and show the user live progress.

## API
- `create` — create a task (`title`, `priority`)
- `depend` — mark a task as depending on another
- `list` / `get_next` — see pending tasks
- `update` — change status to `completed` or `failed`
- `record_execution` — log what you learned

## Notes
- Keep tasks small and independently checkable.
- Same-turn create + complete is valid — the database is always correct even if the UI lags.
- Prefer `update` to `failed`/`completed` over `delete` (delete only when genuinely obsolete).
- Multiple plans can coexist. Use `group_title` on the first `create` of a new plan to label it;
  pass `group_id` on subsequent creates to continue the same plan.
- **All `create`s inside one assistant reply belong to the same plan** — the backend
  auto-joins them into one group (they render as a single card). Only start a NEW plan
  (fresh `group_title`) in a *later* reply when the topic genuinely changes; never
  spawn a new plan per step of the same batch — that floods the UI with one card per step.
- One plan per topic.";

const TASK_TRACKER_GUIDANCE: &str = "\
# Orchestration Engine — Task Tracker (`task_create` / `task_update` / `task_list` / `task_get`)

First-class Task objects for tracking work units.

## API
- `task_create(title, mode, parent_task_id?)` — create a parent task or subtask
- `task_update(task_id, status)` — transition status
- `task_list(status?)` — list tasks
- `task_get(task_id)` — inspect state and children

## State machine
- `pending` → `planning` (Plan mode only) → `in_progress` → `completed` / `failed`
- `ask` mode tasks stay `pending` (read-only advisory, no execution)
- Terminal states: `completed`, `failed`, `archived`

## Notes
- Use parent task + subtasks for decomposition.
- Same-turn create + complete is valid — the database is always correct even if the UI lags.
- Keep task titles short and action-oriented; describe the user's outcome, not your investigation steps.
- Always transition to terminal state — don't leave tasks dangling in `in_progress`.";

/// Always-on: prevents MiniMax / Hermes fine-tunes from emitting
/// filler closers ('等你说话', '(waiting)' ×N, etc.). Kept short —
/// the verbose rules that originally caused the filler pressure
/// (TOOL_EXECUTION_SUMMARY_GUIDANCE, STOPPING_SUMMARY_GUIDANCE)
/// have been removed.
const REPLY_TERMINATION_GUIDANCE: &str = "\
When your reply is done, emit EOS immediately. \
No filler closers ('Done!', '等你说话', '(waiting)', etc.).";

/// Always-on: how to read a user reply that is a bare label.
///
/// Observed failure (real session, 2026-10-05): the model offered
/// A / B / C, the user typed `B` meaning \"write up the bug report\", and
/// the model announced \"你回的是 B\" — then cleaned up temp files instead.
/// The deliverable was never produced. Two errors stacked: the letter was
/// mapped to the wrong option, and the mapping was announced *before*
/// acting, so the user had no chance to correct it. Near the end of a long
/// task \"wrap up\" is by far the strongest contextual prior, and a
/// one-character reply does not out-vote it on its own.
const USER_LABEL_REPLY_GUIDANCE: &str = "\
# Reading a short reply

The user's next message is often a single token — `A`/`B`/`C`, `1`/`2`/`3`, `是`/`否`, `继续`, `M`, `?` — referring to the options **you** listed one message earlier.

- Restate the full option you matched in your FIRST sentence, before any tool call: \"我理解为：〈完整选项〉。\" Announcing the mapping and then doing something else is two mistakes, not one.
- Never pair a decision with cleanup. If the work is unfinished, 收尾 / 清理 / 归档 is not a valid reading of a short reply.
- Make every option self-describing — state its consequence — because the user will reply with the label alone. A bare `A / B / C` is unusable.
- If nothing matches uniquely, ask one direct question. Do not guess.
- If you already have the evidence to decide, decide. Do not hand back a call you could have made.";

// ---------------------------------------------------------------------------
// collect_directions
// ---------------------------------------------------------------------------

use crate::context::{DirRoute, RouteContext, SystemDir};
use crate::prompt_assembly::should_inject_tool_enforcement;
use hermix_intelligence::{preset_for, DomainMode};

/// collect the typed, route-gated directions that will be
/// evaluated against the active route context and rendered into the
/// system prompt. Each direction is one inline `if/else` branch
/// extracted from the previous monolith of `build_system_prompt` —
/// see `context::DirRoute` for the gate logic.
pub fn collect_directions(
    route_ctx: &RouteContext<'_>,
    project_conventions: Option<&str>,
) -> Vec<SystemDir> {
    let user_message = route_ctx.user_message;
    let tool_names = route_ctx.tool_names;
    let model_for_prompt = route_ctx.model_name;
    let mut dirs: Vec<SystemDir> = Vec::new();
    // Formatting discipline (subsequent turns only — first turn gets the
    // full THINKING_DISCIPLINE_GUIDANCE which covers output format).
    if !user_message.is_empty() {
        dirs.push(SystemDir::new(
            "format.short",
            DirRoute::Subsequent,
            format!(
                "## 语气与格式\n用自然语言聊,像真人对话一样有温度、有干劲,不端着。但**最终回答必须用结构化 Markdown 呈现**:凡涉及多步骤、对比、原因/结论、或任何超过三句话的实质内容,使用标题(## / ###)、有序/无序列表、加粗关键词、代码块(```lang)与表格组织层次,让重点一目了然。极简单行回复(如一次性命令确认)可直接说人话。信息密度要高,不啰嗦但绝不漏掉关键点。\n\n\
                {REPORT_SECTIONING}"
            ),
        ));
    }
    // Thinking discipline.
    if user_message.is_empty() {
        dirs.push(SystemDir::new(
            "think.full",
            DirRoute::FirstTurn,
            format!("{THINKING_DISCIPLINE_GUIDANCE}{REPORT_SECTIONING}"),
        ));
    } else {
        dirs.push(SystemDir::new(
            "think.short",
            DirRoute::Subsequent,
            "## Thinking\nBefore your visible reply, emit a `<think>…</think>` block. \
Always close `</think>` before visible text. The think block is for reasoning, not for the reply.",
        ));
    }
    // Tool selection discipline.
    if user_message.is_empty() {
        dirs.push(SystemDir::new(
            "tools.full",
            DirRoute::FirstTurn,
            TOOL_USAGE_GUIDANCE,
        ));
    } else {
        dirs.push(SystemDir::new(
            "tools.short",
            DirRoute::Subsequent,
            "## Tools\nScan tool list before using `terminal`. Use purpose-built tools first.",
        ));
    }
    // Plan-before-execute discipline.
    if user_message.is_empty() {
        dirs.push(SystemDir::new(
            "plan.full",
            DirRoute::FirstTurn,
            PLAN_BEFORE_EXECUTE_GUIDANCE,
        ));
    } else {
        dirs.push(SystemDir::new(
            "plan.short",
            DirRoute::Subsequent,
            "## Plan format\nWhen planning, keep it to one or two natural-language sentences in the reasoning flow. Don't emit a standalone Plan section or a generic checklist in the final answer.",
        ));
    }
    // File-write discipline (J1). Same FirstTurn/Subsequent split as
    // the other discipline blocks: the full rules on the opening turn
    // (they set the frame), a compressed restatement afterwards (the
    // rules still have to hold, but not the whole essay).
    if user_message.is_empty() {
        dirs.push(SystemDir::new(
            "filewrite.full",
            DirRoute::FirstTurn,
            FILE_WRITE_DISCIPLINE,
        ));
    } else {
        dirs.push(SystemDir::new(
            "filewrite.short",
            DirRoute::Subsequent,
            "## Where files may be written\nPlans/designs → `docs/notes/proposed/{class}/`. \
             Throwaway scripts → run in memory via `code_execution`, or `%TEMP%\\hermix-scratch\\`. \
             Never write to the repo root or `docs/` top level. Never touch files you did not create.",
        ));
    }
    // Exploration-efficiency discipline — fixes the "recon-then-act" anti-pattern
    // (listing dirs, fetching file metadata, writing temp scripts before touching
    // the target). Pushed on FirstTurn so the very first exploration turn is
    // disciplined, and on Subsequent so multi-turn analysis keeps the rule.
    // Directly addresses duplicated-directory-listing and serial-only execution.
    if user_message.is_empty() {
        dirs.push(SystemDir::new(
            "explore.full",
            DirRoute::FirstTurn,
            EXPLORE_EFFICIENCY_GUIDANCE,
        ));
    } else {
        dirs.push(SystemDir::new(
            "explore.short",
            DirRoute::Subsequent,
            EXPLORE_EFFICIENCY_SHORT,
        ));
    }
    // Autonomous execution discipline (gated on opt-in).
    dirs.push(SystemDir::new(
        "autonomous.block",
        DirRoute::Autonomous,
        AUTONOMOUS_EXECUTION_GUIDANCE,
    ));
    // Safety guardrail — always-on, prompt-level backstop.
    dirs.push(SystemDir::new(
        "safety.block",
        DirRoute::Always,
        SAFETY_GUIDANCE,
    ));

    // Workbench context — only for sessions bound to a workbench
    // (`workbench_id IS NOT NULL`). Injects manifest + module_index +
    // vendor_index, re-assembled every turn (local dir scan is ms-cheap,
    // so the LLM always sees the latest code state — plan B.2/B.3).
    // Ordinary chats never receive this block (anti-leak, plan R5).
    if let Some(wb_id) = route_ctx.workbench_id {
        match crate::directions::build_workbench_context(wb_id) {
            Ok(ctx_block) => dirs.push(SystemDir::new(
                "workbench.context",
                DirRoute::Always,
                ctx_block,
            )),
            Err(e) => {
                // Degrade gracefully: skip the block, never block the request.
                tracing::warn!(target: "hermix.directions",
                    workbench_id = wb_id, error = %e,
                    "workbench.context assembly failed; skipping direction");
            }
        }
    }

    // Tool-specific guidance blocks, each gated by its own route.
    if tool_names.contains("memory") {
        dirs.push(SystemDir::new(
            "memory.guide",
            DirRoute::HasMemory,
            MEMORY_GUIDANCE,
        ));
    }
    if tool_names.contains("session_search") {
        dirs.push(SystemDir::new(
            "session_search.guide",
            DirRoute::HasSessionSearch,
            SESSION_SEARCH_GUIDANCE,
        ));
    }
    if tool_names.contains("skill_manage") {
        dirs.push(SystemDir::new(
            "skills.guide",
            DirRoute::HasSkillKeyword,
            SKILLS_GUIDANCE,
        ));
    }
    if tool_names.contains("todo") {
        dirs.push(SystemDir::new(
            "todo.guide",
            DirRoute::HasTodo,
            TODO_GUIDANCE,
        ));
    }
    if tool_names.contains("compact_tool_chain") {
        dirs.push(SystemDir::new(
            "context_budget.guide",
            DirRoute::HasBatchKeyword,
            CONTEXT_BUDGET_GUIDANCE,
        ));
    }
    // Orchestration-engine task tracker guidance.
    // Fires when task_create is available, guiding the LLM on task
    // decomposition and same-turn completion (铁律 T).
    if tool_names.contains("task_create") {
        dirs.push(SystemDir::new(
            "task_tracker.guide",
            DirRoute::HasTaskTracker,
            TASK_TRACKER_GUIDANCE,
        ));
    }
    // Reply termination — always-on: prevents filler closers.
    dirs.push(SystemDir::new(
        "reply_termination.always",
        DirRoute::Always,
        REPLY_TERMINATION_GUIDANCE,
    ));
    // Bare-label replies ("B" / "继续" / "?") — always-on. Without this the
    // model has no rule for a one-character user message and falls back to
    // contextual pattern matching, which at the tail of a long task means
    // "wrap up" wins over the letter.
    dirs.push(SystemDir::new(
        "user_label_reply.always",
        DirRoute::Always,
        USER_LABEL_REPLY_GUIDANCE,
    ));
    // Tool failure discipline: prevent blind retry spam.
    dirs.push(SystemDir::new(
        "tool_failure.always",
        DirRoute::Always,
        TOOL_FAILURE_GUIDANCE,
    ));
    // Environment & Shell discipline — always-on: tells the model it's
    // on Windows / Git Bash, how to call PowerShell correctly, and to
    // switch to structured tools when shell fails. Directly addresses
    // the "C 盘清理" failure mode.
    dirs.push(SystemDir::new(
        "env_shell.always",
        DirRoute::Always,
        ENVIRONMENT_SHELL_GUIDANCE,
    ));
    // File lifecycle (deliverable vs scaffolding) — always-on: the model
    // needs to know *before* the first write_file call where a new file is
    // allowed to live. Routing it later would be too late; the file is
    // already on disk by then.
    dirs.push(SystemDir::new(
        "file_lifecycle.always",
        DirRoute::Always,
        FILE_LIFECYCLE_GUIDANCE,
    ));
    // Git safety protocol: prevent destructive git operations.
    dirs.push(SystemDir::new(
        "git_safety.always",
        DirRoute::Always,
        GIT_SAFETY_GUIDANCE,
    ));
    // Skill usage guidance: when and how to use Skills.
    if !tool_names.is_empty() {
        dirs.push(SystemDir::new(
            "skill_usage.always",
            DirRoute::Always,
            SKILL_USAGE_GUIDANCE,
        ));
    }
    // URL safety: prevent LLM from fabricating links.
    dirs.push(SystemDir::new(
        "url_safety.always",
        DirRoute::Always,
        URL_SAFETY_GUIDANCE,
    ));
    // Web & automation capability map — always-on：模型只能从工具表推断
    // 能力，缺这张地图会出现「我没有浏览器能力」/「自动化做成 App」误答。
    dirs.push(SystemDir::new(
        "web_automation.always",
        DirRoute::Always,
        WEB_AUTOMATION_GUIDANCE,
    ));
    // Parallel execution discipline — model-family-agnostic. Previously the only
    // "parallel when safe" nudge lived inside GOOGLE_MODEL_OPERATIONAL_GUIDANCE
    // (gated on GoogleFamily + tool-enforcement whitelist), so non-Google models
    // serialized every call. Now unconditional.
    dirs.push(SystemDir::new(
        "parallel.always",
        DirRoute::Always,
        PARALLEL_EXECUTION_GUIDANCE,
    ));
    // Context compression awareness: explain auto-compaction to the model.
    if user_message.is_empty() {
        dirs.push(SystemDir::new(
            "compression_awareness.first",
            DirRoute::FirstTurn,
            COMPRESSION_AWARENESS_GUIDANCE,
        ));
    }

    // Tool enforcement + model-family extras.
    if !tool_names.is_empty() && should_inject_tool_enforcement(model_for_prompt) {
        dirs.push(SystemDir::new(
            "tool_use.enforce",
            DirRoute::ToolEnforce,
            TOOL_USE_ENFORCEMENT_GUIDANCE,
        ));
        dirs.push(SystemDir::new(
            "google.extras",
            DirRoute::GoogleFamily,
            GOOGLE_MODEL_OPERATIONAL_GUIDANCE,
        ));
        dirs.push(SystemDir::new(
            "openai.extras",
            DirRoute::OpenAiFamily,
            OPENAI_MODEL_EXECUTION_GUIDANCE,
        ));
        // MiniMax-M2 / Hermes fine-tunes share a tendency to emit
        // tool calls as raw XML inside the visible reply rather
        // than through the OpenAI tools field. The streaming
        // redactor in `hermix-server` catches the leak on the wire
        // and the parser in `hermix-core` recognises the wrapper,
        // but the cleanest path is to tell the model up-front
        // which shape the agent actually executes.
        dirs.push(SystemDir::new(
            "minimax.extras",
            DirRoute::MiniMaxFamily,
            MINIMAX_TOOL_FORMAT_GUIDANCE,
        ));
    }
    // Project conventions — non-generic, deployment-specific tuning.
    // Injected only when the active workspace's AGENTS.md exposes a
    // conventions section. Keeps the universal core free of any single
    // user's preferences (see docs/HERMIX_AGENT_SYSTEM_PROMPT.md v2).
    if let Some(conv) = project_conventions {
        if !conv.trim().is_empty() {
            dirs.push(SystemDir::new(
                "project.conventions",
                DirRoute::HasProjectConventions,
                conv,
            ));
        }
    }
    // L1 Domain Mode directive — inject the work-domain-specific
    // guidance block when domain detection has run. The directive
    // text comes from the DomainPreset's `system_directive` field.
    // Chat mode has an empty directive, so we skip it to avoid
    // injecting an empty block.
    if let Some(dm) = route_ctx.domain_mode {
        if dm != DomainMode::Chat {
            let preset = preset_for(dm);
            if !preset.system_directive.is_empty() {
                dirs.push(SystemDir::new(
                    "domain.directive",
                    DirRoute::DomainMode(dm),
                    preset.system_directive,
                ));
            }
        }
    }
    dirs
}

/// Load project-specific conventions from the active workspace's
/// `AGENTS.md`. Returns `None` when there is no workspace, no
/// `AGENTS.md`, or no conventions section — in which case the universal
/// core prompt is used unchanged.
/// The conventions section is matched by a heading containing any of
/// `项目约定` / `project conventions` / `conventions` (case-insensitive),
/// at `#` or `##` level. Everything up to the next heading is returned.
/// This is the data-injection hook that keeps the agent's *universal*
/// behaviour (see `docs/HERMIX_AGENT_SYSTEM_PROMPT.md`) separate from
/// any single deployment's tuning. A different user / workspace simply
/// gets a different `AGENTS.md` — the core never changes.
pub fn load_project_conventions(workspace: &str) -> Option<String> {
    let agents = std::path::Path::new(workspace).join("AGENTS.md");
    let text = std::fs::read_to_string(&agents).ok()?;
    extract_conventions_section(&text)
}

fn extract_conventions_section(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let keywords = ["项目约定", "project conventions", "conventions"];
    let mut start: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        if (t.starts_with("## ") || t.starts_with("# "))
            && keywords.iter().any(|k| t.to_lowercase().contains(k))
        {
            start = Some(i + 1);
            break;
        }
    }
    let start = start?;
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start) {
        let t = line.trim_start();
        if (t.starts_with("## ") || t.starts_with("# ")) && i > start {
            end = i;
            break;
        }
    }
    let section = lines[start..end].join("\n").trim().to_string();
    if section.is_empty() {
        None
    } else {
        Some(section)
    }
}

/// Assemble the `## WORKBENCH CONTEXT` block for a workbench-bound session.
/// Reads manifest + module_index + vendor_index fresh every call — all local
/// disk scans (ms-cheap), so the LLM always sees the latest code state and
/// no cache-invalidation machinery is needed (plan B.2/B.3). Failure returns
/// `Err` and the caller skips the direction instead of blocking the request.
pub(crate) fn build_workbench_context(workbench_id: &str) -> Result<String, String> {
    use hermix_plugin::workbench::{
        format_module_index, module_index, read_manifest, vendor_index,
    };

    let manifest = read_manifest(workbench_id)?;
    let modules = format_module_index(&module_index(workbench_id)?);
    let vendors: Vec<String> = {
        let mut names: Vec<String> = vendor_index()
            .into_keys()
            .collect();
        names.sort();
        names
    };

    let mut b = String::from("## WORKBENCH CONTEXT（自动注入，工作台对话专用）\n\n");
    b.push_str("### 工作台信息\n");
    b.push_str(&format!("- id: {}\n", manifest.id));
    b.push_str(&format!("- name: {}\n", manifest.name));
    if !vendors.is_empty() {
        b.push_str(&format!("- 已装 vendor: [{}]\n", vendors.join(", ")));
    }
    b.push_str(&format!(
        "- 代码目录: {}\n",
        hermix_plugin::workbench::workbench_dir(workbench_id)
            .join("code")
            .display()
    ));
    if !modules.is_empty() {
        b.push_str("\n### 已有模块（复用优先，禁止重写同类功能）\n");
        b.push_str(&modules);
        b.push('\n');
    }
    // 运营上下文（plan 方案 D.2）：业务数据现状，让右栏 LLM 能直接替用户
    // 干活（录数据/汇总）。空库/无库时静默省略，不报错。
    let data_summary = build_workbench_data_summary(workbench_id);
    if !data_summary.is_empty() {
        b.push_str("\n### 业务数据现状（运营上下文，经 wb.db 网关操作）\n");
        b.push_str(&data_summary);
    }
    Ok(b)
}

/// 查工作台 `data/workbench.db` 的业务数据结构：每表 schema + 行数。
/// **不含数据样本**（2026-09-28）—— agent 每写一次库都会让样本变化，
/// 而该块位于系统提示内，直接摧毁 provider 前缀缓存（见下方注释）。
/// 具体数据由 agent 用 db_query 网关工具现查；零上下文膨胀的设计原意不变。
/// 库不存在/为空/查询失败都返回空串（静默降级）。
fn build_workbench_data_summary(workbench_id: &str) -> String {
    use rusqlite::Connection;


    let db_path = hermix_plugin::workbench::workbench_dir(workbench_id)
        .join("data")
        .join("workbench.db");
    if !db_path.exists() {
        return String::new();
    }
    let conn = match Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(target: "hermix.directions",
                workbench_id, error = %e, "workbench data summary: open failed; skipping");
            return String::new();
        }
    };

    let table_names: Vec<String> = match conn.prepare(
        "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    ) {
        Ok(mut stmt) => stmt
            .query_map([], |row| row.get::<_, String>(0))
            .and_then(|rows| rows.collect::<Result<Vec<_>, _>>())
            .unwrap_or_default(),
        Err(e) => {
            tracing::warn!(target: "hermix.directions",
                workbench_id, error = %e, "workbench data summary: table scan failed; skipping");
            return String::new();
        }
    };
    if table_names.is_empty() {
        return String::new();
    }

    let mut b = String::new();
    for table in &table_names {
        // 表名来自 sqlite_master 自身，非用户输入，但引号包裹仍属一致性纪律。
        let count: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM \"{table}\""),
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let schema: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name = ?1",
                [table],
                |row| row.get(0),
            )
            .unwrap_or_default();
        b.push_str(&format!("- 表 {table}：{count} 行\n"));
        if !schema.is_empty() {
            b.push_str(&format!("  schema: {schema}\n"));
        }
        if count > 0 {
            // 2026-09-28: 末 N 行样本从系统提示移除。此前 agent 每写一次
            // 数据，样本就变一次 → 该块位于 WORKBENCH CONTEXT（系统提示内）
            // → provider 自动前缀缓存从该字节起全部 miss（tools + 整段
            // 消息历史每 turn 重付全价）。schema + 行数在会话内稳定；
            // 具体数据按需用 db_query 网关工具现查——与"零上下文膨胀"
            // 的设计原意一致。
            b.push_str("  (具体数据用 db_query 现查，不在系统提示内预取)\n");
        }
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_conventions_section_by_heading() {
        let md = "# Title\n\n## Conventions\nrule one\nrule two\n\n## Build\ncargo check\n";
        let got = extract_conventions_section(md).unwrap();
        assert!(got.contains("rule one"));
        assert!(got.contains("rule two"));
        assert!(!got.contains("cargo check"));
    }

    #[test]
    fn extracts_chinese_conventions_heading() {
        let md = "intro\n\n## 项目约定\n- 无蓝紫\n- sidecar cp\n\n## 其他\nx\n";
        let got = extract_conventions_section(md).unwrap();
        assert!(got.contains("无蓝紫"));
        assert!(!got.contains("x"));
    }

    #[test]
    fn no_conventions_section_returns_none() {
        let md = "# Title\n## Build\ncargo check\n";
        assert!(extract_conventions_section(md).is_none());
    }

    #[test]
    fn collect_directions_injects_conventions_when_present() {
        let mut tn = std::collections::HashSet::new();
        tn.insert("todo");
        let ctx = RouteContext {
            user_message: "",
            tool_names: &tn,
            model_name: "gpt-4o",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: None,
            workbench_id: None,
        };
        let dirs = collect_directions(&ctx, Some("禁用蓝紫配色"));
        assert!(dirs.iter().any(|d| {
            d.tag == "project.conventions" && d.body.contains("禁用蓝紫配色")
        }));
    }

    #[test]
    fn collect_directions_skips_conventions_when_absent() {
        let tn = std::collections::HashSet::new();
        let ctx = RouteContext {
            user_message: "",
            tool_names: &tn,
            model_name: "gpt-4o",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: None,
            workbench_id: None,
        };
        let dirs = collect_directions(&ctx, None);
        assert!(!dirs.iter().any(|d| d.tag == "project.conventions"));
    }

    #[test]
    fn collect_directions_injects_domain_directive_for_code() {
        let tn = std::collections::HashSet::new();
        let ctx = RouteContext {
            user_message: "帮我修复 main.rs",
            tool_names: &tn,
            model_name: "gpt-4o",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: Some(DomainMode::Code),
            workbench_id: None,
        };
        let dirs = collect_directions(&ctx, None);
        assert!(dirs.iter().any(|d| {
            d.tag == "domain.directive" && d.body.contains("代码开发模式")
        }), "should inject Code domain directive");
    }

    #[test]
    fn collect_directions_injects_domain_directive_for_research() {
        let tn = std::collections::HashSet::new();
        let ctx = RouteContext {
            user_message: "搜索一下",
            tool_names: &tn,
            model_name: "gpt-4o",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: Some(DomainMode::Research),
            workbench_id: None,
        };
        let dirs = collect_directions(&ctx, None);
        assert!(dirs.iter().any(|d| {
            d.tag == "domain.directive" && d.body.contains("信息研究模式")
        }), "should inject Research domain directive");
    }

    #[test]
    fn collect_directions_skips_domain_directive_for_chat() {
        let tn = std::collections::HashSet::new();
        let ctx = RouteContext {
            user_message: "你好",
            tool_names: &tn,
            model_name: "gpt-4o",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: Some(DomainMode::Chat),
            workbench_id: None,
        };
        let dirs = collect_directions(&ctx, None);
        assert!(!dirs.iter().any(|d| d.tag == "domain.directive"),
            "Chat mode has empty directive, should not inject");
    }

    #[test]
    fn collect_directions_skips_domain_directive_when_none() {
        let tn = std::collections::HashSet::new();
        let ctx = RouteContext {
            user_message: "hello",
            tool_names: &tn,
            model_name: "gpt-4o",
            autonomous_mode: false,
            tool_call_count: 0,
            domain_mode: None,
            workbench_id: None,
        };
        let dirs = collect_directions(&ctx, None);
        assert!(!dirs.iter().any(|d| d.tag == "domain.directive"),
            "No domain detection → no domain directive");
    }
}
