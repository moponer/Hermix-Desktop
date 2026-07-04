//! Core agent loop engine.
//!
//! The `AgentLoop` orchestrates the autonomous agent cycle:
//! 1. Send messages + tools to the LLM
//! 2. If the LLM responds with tool calls, execute them (in parallel)
//! 3. Append results to conversation history
//! 4. Repeat until the model finishes naturally or the turn budget is exceeded

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hermix_focus::{
    extract_time_window, FocusFrame, FocusStack, MemoryHit, MemoryLayer, MemoryQuery,
    SessionFrame, SessionFrameStatus,
};
// `chrono` (DateTime/Utc) dropped on 2026-07-04 when `OAuthStoreCredential`
// moved to `crate::oauth_refresh`.
use futures::StreamExt;
// serde::Serialize / serde::Deserialize were dropped on 2026-07-04 when
// `OAuthStoreCredential` moved to `crate::oauth_refresh`; this file no
// longer holds any #[derive(Serialize, Deserialize)] structs.
use serde_json::{json, Value};
use tokio::task::JoinSet;
use tokio::time::sleep;

use hermix_core::{
    format_token_count, AcuiDirective, AgentError, AgentResult, LlmProvider, LlmResponse, Message,
    MessageRole, StreamChunk, ToolCall, ToolCallSanitizer, ToolResult, ToolSchema,
    UsageStats,
};
use hermix_intelligence::{
    prefetch::Prefetcher,
    AlwaysInclude, KeywordRouter, ToolSchemaRouter, TurnContext,
};

use crate::archive::{archive_context_section, render_archive_summary, ArchiveRef};
use crate::budget;
use crate::context::{
    load_builtin_memory_snapshot, load_soul_md, resolve_personality, ContextManager,
    SystemPromptBuilder,
};
use crate::credential_pool::CredentialPool;
use crate::interrupt::InterruptController;
use crate::plugins::{HookResult, HookType, PluginManager};
use crate::python_alignment::{
    budget_pressure_text, inject_budget_pressure_into_last_tool_result,
    sanitize_surrogates, strip_budget_warnings_from_messages,
    strip_think_blocks_for_ack, ThinkBlockParser,
};
use crate::smart_model_routing::TurnRouteSignature;
pub use crate::smart_model_routing::{ApiMode, CheapModelRouteConfig, SmartModelRoutingConfig};
use crate::steer::{
    apply_pending_steer_to_last_tool_message, concat_pending, normalize_accepted_steer_text,
};
use tracing::{info, instrument};

// ---------------------------------------------------------------------------
// Backwards-compat re-exports (P2.2.4, 2026-06-30)
// ---------------------------------------------------------------------------
//
// `AgentConfig`, `RetryConfig`, and `RuntimeProviderConfig` moved to
// `crate::agent_config` on 2026-06-30 (REFACTOR_PLAN §4). Two test files
// still import via `hermix_agent::agent_loop::AgentConfig`, so we keep
// these `pub use` aliases live — same source-of-truth, just a thinner
// alias to the new module.
//
// `ToolEntry`, `ToolRegistry`, `TurnMetrics`, `EvolutionCounters`,
// `ToolDispatchDecision`, `AgentCallbacks`, `ErrorClass`, the helper
// functions, and `TOOL_RESULT_CHAR_LIMIT` were extracted to
// `crate::agent_helpers` on 2026-06-30 (P2.2.5). Same source-of-truth,
// re-exported below so existing callers (e.g.
// `hermix_agent::agent_loop::ToolRegistry` in hermix-cron and the test
// files) keep working unchanged.

pub use crate::agent_config::{AgentConfig, RetryConfig, RuntimeProviderConfig};
pub use crate::agent_helpers::{
    classify_error, classify_tool_error, compute_prompt_cache_key, default_max_autonomous_nudges,
    jittered_backoff, rand_u64_range, AgentCallbacks,
    ErrorClass, EvolutionCounters, ToolDispatchDecision, ToolEntry, ToolErrorClass, ToolRecovery,
    ToolRegistry, TurnMetrics,
};

// ---------------------------------------------------------------------------
// OAuth credential + refresh manager (extracted 2026-07-04)
// ---------------------------------------------------------------------------
//
// `OAuthRefresh` + `OAuthStoreCredential` moved to `crate::oauth_refresh`.
// Kept as `pub use` aliases so `hermix_agent::agent_loop::OAuthRefresh`
// remains a valid import path for any external consumer (e.g. hermix-cron,
// test files). The implementation now lives next to the HTTP code it
// orchestrates (reqwest POST, on-disk JSON store, per-provider backoff)
// — see `oauth_refresh.rs` for the full surface and unit tests.

pub use crate::oauth_refresh::{OAuthRefresh, OAuthStoreCredential};

const MEMORY_GUIDANCE: &str = "You have persistent memory across sessions. Save durable facts using the memory tool: user preferences, environment details, tool quirks, and stable conventions. Memory is injected into every turn, so keep it compact and focused on facts that will still matter later. Prioritize what reduces future user steering. Do NOT save task progress, session outcomes, completed-work logs, or temporary TODO state to memory.";

const SESSION_SEARCH_GUIDANCE: &str = "When the user references something from a past conversation or you suspect relevant cross-session context exists, use session_search to recall it before asking them to repeat themselves.";

const SKILLS_GUIDANCE: &str = "After completing a complex task (5+ tool calls), fixing a tricky error, or discovering a non-trivial workflow, save the approach as a skill with skill_manage so you can reuse it next time. When using a skill and finding it outdated or incomplete, patch it immediately with skill_manage(action='patch').\n\nP0-1 (auto-skill learning loop): when saving a learned workflow, prefer `skill_manage(action='auto_extract_from_conversation')` over the bare `create` action. It enforces a quality gate (name 3–80 chars, summary >10 chars and non-placeholder, ≥1 concrete step), writes SKILL.md frontmatter so the orchestrator indexes the skill cleanly, and dedupes — if a skill with the same name already exists, it updates instead of creating a duplicate. Pass `name` (kebab-case slug), `summary` (one paragraph >10 chars), `steps` (ordered array of concrete actions), and optionally `rationale` ('why this works'). Do NOT write vague or empty skills — the gate will reject them and you'll waste a turn. If nothing genuinely reusable emerged from the task, do nothing — the background review only fires when there's signal.";

const CONTEXT_BUDGET_GUIDANCE: &str = "P0-2 (zero-context RPC): when you need to perform 2+ tool calls in a row where the results are **gathered for context** (read N files, query N APIs, list N skills) and **not used to branch** mid-flight, pack them into a single `compact_tool_chain` call. The chain collapses the N tool_results into one LLM-context entry — this is the main lever for keeping long sessions under the context budget (see `crates/hermix-agent/src/compression.rs::auto_compress_if_over_threshold`).\n\nWhen to use `compact_tool_chain`:\n- Reading 2–32 files for one summary / analysis / grep.\n- Querying 2–32 independent APIs in parallel.\n- Listing/searching across multiple tool namespaces in one go.\n\nWhen NOT to use it:\n- You need to **branch** on a tool result (e.g. 'if file X exists, take path A; else path B') — issue separate tool calls.\n- You need to retry a step after seeing the previous step's output — chain steps don't feed into each other.\n- You need streaming progress to the user — the chain only reports at the end.\n\nSchema: `{chain: [{tool, params, label?}, ...], mode?: 'sequential'|'parallel', failure_mode?: 'abort_on_first_error'|'continue', intent?: string}`. Chain must be 2–32 steps. Mode defaults to 'sequential'; failure_mode defaults to 'abort_on_first_error'. Per-step results are truncated to 4000 chars each — if you need the full output, use the tool directly with no chain wrapper.";

const USER_STYLE_GUIDANCE: &str = "P2-6 (dialectic user style): the system writes a `user.style:<style>` fact per turn (technical | direct | exploratory | mixed) with a confidence score. When the recognized facts surface one of these tags, **adapt your response**:\n- `technical` → skip 'in plain English' restatements, lead with the code/spec, drop caveats the user already knows.\n- `direct` → no preamble, no 'happy to help', short paragraphs, mirror the imperative tone.\n- `exploratory` → it's OK to think out loud, surface tradeoffs, ask 1 clarifying question if the path is genuinely ambiguous.\n- `mixed` → keep the default style; the detector didn't commit.\n\nDo NOT announce that you've detected the style, and do NOT mention the fact row — just behave accordingly. If the style fact contradicts the current turn's obvious shape (e.g. `direct` fact but the user just wrote a 200-char exploratory question), trust the current turn.";

const TOOL_USAGE_GUIDANCE: &str = r#"
# Tool selection discipline

Before calling a generic tool like `terminal`, `process_registry`, or `execute_code`, **scan the available tool list** for a purpose-built alternative. Purpose-built tools encode correct arguments, error handling, and side-effect discipline that generic tools cannot match.

## Decision pattern
1. The user mentions a specific file type, service, or action → look for a tool whose name/description matches (e.g. `.pdf` → `read_pdf`, GitHub → `github_*`, image → `vision_analyze`).
2. If a matching tool exists, **call it first**. Do not chain `terminal` calls to re-implement what the tool already does.
3. Only fall back to `terminal` / `process_registry` when no purpose-built tool covers the task, or when the user explicitly asks for raw shell access.

## Absolute rule — when the user provides a file path, call the right tool FIRST
When the user message contains:
- An explicit file path (e.g. `G:\…\通知.pdf`, `/home/user/file.txt`, `C:\code\main.rs`)
- A file name or extension (`.pdf`, `.md`, `.json`)
- A request to "read", "write", "draft", "create", "analyze" a file

**You MUST call the appropriate tool (`read_pdf` for PDFs, `read_file` for text/code, `write_file` for creating files, etc.) using that EXACT path — do not use `terminal` or `list_files` to search for the file first.**

The user already told you where the file is. `terminal` is for *navigation* and *shell operations*, not for replacing purpose-built I/O tools.

## Anti-pattern — the "探索式盲目搜索" trap (必须避免!)
当用户消息中包含明确文件路径或文件名时，**绝不允许**使用 `terminal` 命令来"找文件"或"验证路径"。如果用户提供了路径，直接使用 `read_file`/`read_pdf` 等对应工具调用即可。

以下行为属于典型反模式：
- 用户发了 `G:\xxx\file.pdf` → agent 用 `cd G:` + `dir` 找路径 → ❌ 直接使用 `read_pdf path="G:\xxx\file.pdf"`
- 用户发了文件名 → agent 用 `search_files` 或 `ls` 猜测路径 → ❌ 直接用文件路径调用对应工具
- 连续 4+ 次用 `terminal` 尝试不同路径变体 → ❌ 放弃猜测，声明无法找到该文件或联系用户确认

**当同类工具连续失败 2 次后必须切换策略**。如果直接调用工具失败，不要反复用 terminal 尝试猜测路径，应该直接回复用户说明文件问题。

## Visual / browser / GUI automation requests
When the user says "看下 X 下有什么" / "帮我打开浏览器" / "帮我操作网页" / "截图" / any browser or GUI interaction phrasing, **use the browser tools** (`browser_navigate`, `browser_snapshot`, `browser_click`, `browser_type`, `browser_scroll`) or `computer_use` as appropriate.

- For **file listing / reading local files**, prefer `list_files` / `read_file` — they are purpose-built and precise.
- For **web browsing / page interaction / GUI automation**, use `browser_*` tools or `computer_use` — these are the purpose-built tools for these tasks.
- `computer_use` covers desktop GUI interactions (clicking buttons in apps, typing into windows, interacting with dialogs). Use it when browser tools cannot reach the target (native desktop apps, system dialogs).
- `browser_*` tools cover web page interactions (navigating, clicking, typing, scrolling in web pages). Prefer these over `computer_use` for web tasks.

## Anti-patterns (do not do)
- "Let me try the terminal first to see what's available..." — wrong, scan the tool list first.
- "There's no PDF tool so I'll run pdftotext via terminal..." — wrong, check again; `read_pdf` exists.
- Chaining 4+ tool calls of the same family when each is failing — switch strategy or declare inability.
- Re-implementing in `process_registry` what a purpose-built tool already provides.
- Using `terminal` to "find" a file that the user already gave you the exact path for.

## Forbidden shapes in `terminal` arguments (do not invent these)

The Tirith pre-check engine **blocks** tool calls whose arguments match any of the following shapes. You will receive a structured error and a chance to reformulate, but the user will not be asked to bless a command that violates these rules — treat them as compile-time errors in your output, not as edge cases to negotiate.

- **Windows raw path namespace** — never write `\\?\D:\...`, `\\?\UNC\server\...`, or `\\.\COM1` in a `terminal` `command`. Use the normal form (`D:\data`, `\\server\share\file`). The `\\?\` prefix is for Win32 kernel / device-driver code; it has no semantic in user shell commands and the OS will often interpret it as a literal path under the current drive's `\\?\` directory, not as the long-path escape hatch.
- **System directories** — do not `cat`, `type`, `dir`, `find`, `ls`, or otherwise shell-read `C:\Windows\System32`, `C:\Windows\SysWOW64`, `C:\Windows\WinSxS`, or `C:\Windows\assembly`. These are managed by Windows Update and contain no user data. Use the `read_file` tool against the user-supplied workspace path instead.
- **Shell-injection shapes** — do not begin a `command` with a flag-like prefix immediately followed by a metacharacter (e.g. `-n && rm`, `-e | sh`). The first whitespace-separated token of a shell command is almost always the program name; if you're tempted to lead with a flag, the program is probably wrong.
- **Filesystem-root operations** — `find / -delete`, `rm /*`, `chmod -R 777 /` and similar all require explicit user approval even if the user typed a permissive command earlier. If a recursive filesystem-root op is genuinely needed, say so in chat and let the user invoke it themselves.

**Why these are hard blocks and not just warnings**: the LLM has been observed to fabricate `\\?\` paths and other pathological shapes that the user never typed. Asking the user to "allow" a command the model invented is the wrong default — the user has not evaluated whether the command is sensible. Hard-block and let the model reformulate against real paths the user mentioned.
"#;

const PLAN_BEFORE_EXECUTE_GUIDANCE: &str = "\
# Plan before you execute (mandatory)

Before calling ANY tool in response to a user request, you MUST first output a brief execution plan (max 3 lines). \
The plan must state:
1. **Target deliverable** — exactly what you will deliver to the user (e.g. \"a complete internal safety-month notice with 3 attachments\")
2. **Tool(s) to call** — by name and what each does toward the deliverable
3. **Expected outcome** — what you expect to get back from each tool and how it contributes to the deliverable

After the plan, proceed to make the actual tool call(s) in the same response.

## Rules
- **One plan per user request**, not per tool call. If a request needs 3 tool calls, write one plan that covers all 3.
- **Max 3 lines.** Bullet points or a short numbered list are preferred.
- **If the user's request is trivial** (e.g. \"hello\", \"what time is it\", a yes/no question), you may skip the plan.
- **If a tool fails**, produce a revised plan before calling an alternative tool — do not blindly retry the same call.
- **Keep the deliverable in focus** — every tool call must be justified as necessary for the stated deliverable. If you find yourself calling tools that don't contribute to it, you've lost the plan.

## Example
```
User: 根据 G:\\通知.pdf 起草一份方案

Plan:
最终输出：一份企业内部安全生产月通知活动方案（含活动主题、时间节点、实施步骤）
1. read_pdf → 读取通知.pdf 获取原文要求
2. 提取关键时间节点和责任分工
3. 组合起草 → 输出完整通知方案

Response: [tool call to read_pdf]
```

This is a compulsory discipline for all tool-using agents. It reduces wasted turns, \
helps you converge faster, and lets the user see your reasoning before execution.";

// Python `AIAgent._MEMORY_REVIEW_PROMPT` / `_SKILL_REVIEW_PROMPT` / `_COMBINED_REVIEW_PROMPT` (v2026.4.16)
const MEMORY_REVIEW_PROMPT: &str = "Review the conversation above and consider saving to memory if appropriate.\n\n\
Focus on:\n\
1. Has the user revealed things about themselves — their persona, desires, preferences, or personal details worth remembering?\n\
2. Has the user expressed expectations about how you should behave, their work style, or ways they want you to operate?\n\n\
If something stands out, save it using the memory tool. \
If nothing is worth saving, just say 'Nothing to save.' and stop.";

const SKILL_REVIEW_PROMPT: &str =
    "Review the conversation above and consider saving or updating a skill if appropriate.\n\n\
Focus on: was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome?\n\n\
**P0-1 (auto-skill learning loop)**: when you decide a skill is worth saving, \
prefer `skill_manage(action='auto_extract_from_conversation')` over `auto_create`. \
It enforces a quality gate (name 3–80 chars, summary >10 chars and non-placeholder, \
≥1 step) and writes SKILL.md frontmatter so the orchestrator picks the skill up \
cleanly. It also dedupes — if a skill with the same name exists, it updates instead \
of creating a duplicate.\n\n\
Provide a `name` (kebab-case slug, 3–80 chars), a `summary` (one paragraph, >10 \
chars), an ordered `steps` array (each step a concrete actionable instruction), \
and optionally a `rationale` explaining why this is worth remembering next time. \
If a relevant skill with the same name already exists, the dedup logic will route \
your call to an update automatically — just call `auto_extract_from_conversation`.\n\n\
If nothing is worth saving, just say 'Nothing to save.' and stop.";

const COMBINED_REVIEW_PROMPT: &str = "Review the conversation above and consider two things:\n\n\
**Memory**: Has the user revealed things about themselves — their persona, \
desires, preferences, or personal details? Has the user expressed expectations \
about how you should behave, their work style, or ways they want you to operate? \
If so, save using the memory tool.\n\n\
**Skills**: Was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome? **Prefer \
`skill_manage(action='auto_extract_from_conversation')`** — it enforces a quality \
gate, writes SKILL.md frontmatter, and dedupes against existing skills by name. \
Pass `name` (3–80 chars), `summary` (>10 chars), `steps` (≥1), and optionally \
`rationale`.\n\n\
Only act if there's something genuinely worth saving. \
If nothing stands out, just say 'Nothing to save.' and stop.";

const TOOL_USE_ENFORCEMENT_GUIDANCE: &str = "# Tool-use enforcement\nYou MUST use your tools to take action. Do not describe what you would do without actually doing it. When you say you will perform an action, make the corresponding tool call in the same response. Every response should either (a) contain tool calls that make progress, or (b) deliver a final result.";

/// Tool-selection discipline for browser automation. Without this block, the
/// model falls back to `web_fetch` (a stateless HTTP scrape) when the browser
/// reports "page still loading" and spirals into 5+ identical calls. The rule
/// is: if a `browser_*` tool exists for the job, use it. `web_fetch` is only
/// for pages that don't require JS execution.
const BROWSER_TOOL_SELECTION_GUIDANCE: &str = "\
# Browser tool selection (mandatory)

When the user's task involves a website that needs interaction (login, captcha, \
form submit, click, scroll, type, screenshot-read), you MUST use the `browser_*` \
tool family — NOT `web_fetch` / `获取网页`. The two are not interchangeable:

- `web_fetch` / `获取网页` — stateless HTTP scrape. Gets raw HTML / markdown. \
  Cannot execute JS, cannot interact, cannot read captcha images, cannot wait \
  for dynamic content, does NOT share cookies / storage with the browser.
- `browser_navigate` — opens a real browser (CDP) at the URL and waits for \
  `Page.loadEventFired`. Returns `{load_observed: true|false, hint}`.
- `browser_snapshot` — returns the accessibility tree as a compact YAML tree \
  with `[ref=eN]` handles you can pass straight to `browser_click` / `browser_type`.
- `browser_wait_for` — polls for a CSS selector (e.g. `form`, `input[name=captcha]`) \
  up to a timeout. Use this whenever the page is still loading, when you see \
  a captcha, or when `browser_snapshot` returns an empty / stale tree.
- `browser_vision` — last resort, sends the current page as a PNG to a vision \
  model. Use only for image-only information (captcha glyphs, captioned images, \
  Canvas-rendered charts) that the accessibility tree cannot describe.
- `browser_click` / `browser_type` — accept either a CSS selector or `[ref=eN]`.

## Hard rules
1. **Never call `web_fetch` after a `browser_*` call.** Once a task has touched \
   the browser, stay in the browser. `web_fetch` will give you a different \
   document (no JS, no cookies, no localStorage) and the loop will diverge.
2. **If `browser_navigate` returns `load_observed: false`, call \
   `browser_wait_for`** with a selector that proves the page is interactive \
   (e.g. `form`, `#username`, `input[name=captcha]`), THEN \
   `browser_snapshot`. Do NOT loop `browser_snapshot` blindly.
3. **For captchas: `browser_wait_for` for the captcha image, then \
   `browser_vision` to read it.** The vision model sees the actual pixels \
   and gives a precise OCR.
4. **For form fills: `browser_snapshot` once → use the `[ref=eN]` handles \
   with `browser_type` / `browser_click`.** Do not write CSS selectors \
   from memory.
5. **If a tool returns the same string twice in a row, you have the wrong \
   tool.** Stop and switch (e.g. snapshot → wait_for → snapshot).
";

/// Injected when `AgentConfig::autonomous_mode = true`.
///
/// This block instructs the LLM to exhaust all tool-based approaches before
/// stopping and asking the user for input. The loop itself mirrors this by
/// re-injecting a "keep going" system message whenever the model outputs
/// plain text without tool calls while turns remain.
const AUTONOMOUS_EXECUTION_GUIDANCE: &str = "\
# Autonomous execution (mandatory)

You are operating in **autonomous mode**. Your directive is to complete the \
user's goal entirely on your own, without stopping mid-task to ask the user \
for clarification or missing information.

## Rules
1. **Never stop to ask the user for information that a tool call could retrieve.** \
   If you need credentials, page content, file data, or any other value — call a \
   tool to obtain it first. Only ask the user if every tool-based path has been \
   exhausted and you truly cannot proceed.
2. **Prefer trying over asking.** If you are uncertain whether an approach will \
   work, try it with a tool call. Do not ask for permission before attempting.
3. **On tool failure, adapt immediately.** If a tool returns an error or unexpected \
   result, revise your plan and call a different tool. Do not output a failure \
   summary and stop — keep working.
4. **The only valid stopping condition is task completion or genuine impossibility.** \
   You may stop and address the user only when (a) the task is fully done, or \
   (b) you have tried every available tool and none can make further progress — \
   in which case clearly explain what was attempted and what is blocking you.
5. **Do not ask for confirmation before taking reversible actions.** Click buttons, \
   fill forms, navigate pages, write files — just do it. Reserve confirmation \
   requests for irreversible or destructive operations only.
6. **If you lack a required value (e.g. a password), look for it on-screen, in \
   context, or via a tool before asking the user.** Scroll, screenshot, read files, \
   search memory — try everything first.
7. **When you say \"let me use X\" you MUST emit the tool call in the same turn.** \
   Saying \"let me use vision\" or \"let me try a snapshot\" without an actual \
   tool call in that turn will not advance the task — the loop will treat it as a \
   stop and you will be re-prompted. If you announce a tool, call it.
8. **Same tool returning the same output 2+ times in a row = stuck.** Vary the \
   arguments, try a different tool, scroll the page, wait for loading, change the \
   selector — do NOT keep calling the same tool with the same args.";

/// System-prompt block injected for MiniMax / MiniMax-style models.
///
/// ## Why this exists
///
/// MiniMax-M2, MiniMax / Hermes fine-tunes, and several other
/// tool-call-friendly open-weight models expect tool calls to be
/// emitted as raw XML wrapped in `<minimax:tool_call>…</minimax:tool_call>`
/// (the model's native format) rather than through the OpenAI `tools`
/// API field. If we don't tell the model to use a recognised shape, it
/// either:
///   (a) emits the tool call as unstyled text the user can see, then
///       stalls while the agent loop retries the unparseable response,
///       or
///   (b) falls back to its own internal "tool format" that the
///       `HermesToolCallParser` doesn't recognise, leaving the
///       tool call unexecuted.
///
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
```";

const FORMATTING_DISCIPLINE_GUIDANCE: &str = "# Output formatting discipline\n\
Markdown is for **structure**, not emphasis. Chat UIs render every `**...**` as visible bold, and over-bolding creates visual noise that hurts readability.\n\n\
**DO** use `**bold**` for short technical tokens the user scans for:\n\
  • Section labels that introduce a list (`**Color tokens:**`, `**Summary:**`)\n\
  • Code identifiers, file paths, command names, error codes, API names\n\
  • UI element names when describing them (`the **Send** button`)\n\n\
**DO NOT** use bold for:\n\
  • Whole sentences, paraphrased reasoning, value statements, slogans\n\
  • Personal names, brand names, or quoted phrases from the user\n\
  • Generic emphasis in narrative prose (the meaning already comes through)\n\n\
For structure, use markdown headings, lists, and code blocks — not bold. \
If a section needs separation, add a blank line or a heading.";

const THINKING_DISCIPLINE_GUIDANCE: &str = "# Output format — strict\n\
You MUST structure every reply as exactly two parts, in this order:\n\n\
1. `<think>...</think>` — your internal reasoning. Private scratch space; the user never sees it.\n\
2. The user's reply, starting on the very next line after `</think>`.\n\n\
## Hard rules\n\
- The `<think>...</think>` block must contain **only** reasoning. Never start the user's reply inside the think block.\n\
- Never quote the user's words inside `<think>` as a way to begin the reply.\n\
- The first character after `</think>` is the first character of the user's reply.\n\
- If you have no reasoning to share, still emit `<think></think>` (an empty block is fine).\n\
- Use only `<think>...</think>` — never `<thinking>...</thinking>` or any other variant.\n\
- The reply's language must match the user's question.\n\n## CRITICAL — keep the think block short and always close it\n\n- The `<think>` block should normally be 1–6 short lines. If you find yourself writing more than ~10 lines of reasoning, **stop and condense** — capture the next decision step only.\n- **Every `<think>` MUST be closed with `</think>` before any reply text appears.** Forgetting the close tag causes the renderer to swallow the rest of your reply as invisible reasoning, and the user will see no answer at all.\n- After `</think>`, the next character (or newline) begins the visible reply. There is no second closing step.\n- Inside `<think>`, write plain text only — no markdown, no headings, no code fences, no bullet lists. The reasoning block is rendered as preformatted plain text.\n\n\
## Correct example\n\
`<think>The user greeted in Chinese. I should reply in Chinese with a friendly greeting and offer to help.</think>`你好!有什么可以帮到你的?\n\n\
## Wrong example (do not do this)\n\
`<think>The user said \"你好\". Let me respond. 你好!有什么可以帮到你的?</think>` — the reply started inside the think block, which is forbidden.";

const OPENAI_MODEL_EXECUTION_GUIDANCE: &str = "# Execution discipline (OpenAI)\nUse tools whenever they improve correctness, completeness, or grounding. Do not stop early when another tool call would materially improve the result. Verify outcomes before declaring completion.";

const GOOGLE_MODEL_OPERATIONAL_GUIDANCE: &str = "# Operational guidance (Google)\nBe concise and execution-first. Prefer absolute paths, parallel tool calls when safe, and verify each substantive change.";

/// Result of collecting one streaming completion (may end with user interrupt).
enum StreamCollectOutcome {
    Complete(LlmResponse),
    Interrupted(LlmResponse),
}

// ---------------------------------------------------------------------------
/// The main agent loop.
///
/// Owns the configuration, a tool registry, and an LLM provider.
/// Call `run()` or `run_stream()` to begin an autonomous loop.
pub struct AgentLoop {
    pub config: AgentConfig,
    pub tool_registry: Arc<ToolRegistry>,
    pub llm_provider: Arc<dyn LlmProvider>,
    pub interrupt: InterruptController,
    /// Optional plugin manager for lifecycle hooks.
    pub plugin_manager: Option<Arc<std::sync::Mutex<PluginManager>>>,
    /// Callbacks for progress reporting.
    pub callbacks: Arc<AgentCallbacks>,
    /// Sub-agent delegation depth (0 = root).
    pub delegate_depth: u32,
    /// Primary LLM credential pool (Python `primary["credential_pool"]` / runtime pool).
    pub primary_credential_pool: Option<Arc<CredentialPool>>,
    /// Memory/skill nudge counters (persist for the lifetime of this `AgentLoop`).
    pub evolution_counters: Arc<Mutex<EvolutionCounters>>,
    /// Manager for runtime-provider OAuth token refresh + backoff state.
    pub oauth_refresh: OAuthRefresh,
    /// Optional in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are executed by the orchestrator (spawn/timeout/cancel/
    /// lineage) instead of simply returning a signal envelope.
    sub_agent_orchestrator: Option<Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>>,
    /// Mid-run `/steer` buffer (Python `AIAgent._pending_steer`).
    pending_steer: Arc<Mutex<Option<String>>>,
    /// Optional tool schema router. When set, the LLM only sees the routed
    /// subset of `tool_registry` (Phase 7 H1 — token saving, see
    /// `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §3). When `None`, the full
    /// registry is sent (legacy behavior).
    pub tool_router: Option<Arc<dyn ToolSchemaRouter>>,
    /// Phase 15: Time-aware recall. When set, `memory_prefetch`
    /// extracts a `time_window` from the user query, fans out to all
    /// `MemoryLayer` backends through the router, deduplicates and
    /// re-ranks hits by salience, and injects the resulting structured
    /// blocks as a `## Memory` section of the system prompt. This is
    /// the **only** read path; the legacy `MemoryManager::prefetch_all`
    /// markdown path was removed in Phase 16.
    ///
    /// Phase 17: the **write** path is also wired through this
    /// struct's sibling `plugin_manager: PluginManager`. The seven
    /// write-path helpers (`memory_sync`, `notify_memory_writes`,
    /// `notify_delegations`, `memory_on_turn_start`,
    /// `memory_system_prompt`, `memory_pre_compress_note`,
    /// `memory_on_session_end`) iterate
    /// `MemoryRouter` to fan out to each
    /// bundled `MemoryProviderPlugin`.
    pub memory_router: Option<Arc<hermix_focus::MemoryRouter>>,
    /// Always-include whitelist (read_file/write_file/memory by default).
    pub always_include: Vec<String>,
    /// Phase 15 H2: Intent-aware prefetcher. When set, the agent loop
    /// classifies the user message into a `PrefetchIntent` *before*
    /// the LLM call, dispatches read-only (Safe) tools in parallel,
    /// and injects results as a system-prompt block. This reduces
    /// the time-to-first-token by overlapping tool execution with
    /// LLM reasoning.
    pub prefetcher: Option<Arc<Prefetcher>>,
    /// Learned prefetch stats (persisted across turns/sessions).
    learned_prefetch_stats: Arc<Mutex<hermix_intelligence::prefetch::IntentStats>>,
    /// M1: per-session `FocusStack<SessionFrame>`. The agent loop
    /// pushes a frame on every `delegate_task` invocation and pops
    /// it on completion; a `FocusConclusion` is auto-generated and
    /// fed to the memory router for long-term indexing. See
    /// `docs/PROPOSAL_BAILONGMA_INSPIRED.md` §2.2 + M1.
    ///
    /// The stack starts empty; callers push the session root
    /// (one frame per session) at `run()` start via
    /// `note_session_started`, and sub-agents push/pop via
    /// `note_delegation_start` / `note_delegation_end`.
    pub focus_stack: Arc<Mutex<FocusStack<SessionFrame>>>,
    /// M1.6.3: optional consciousness tracker for broadcasting
    /// `focus_depth` changes when the focus stack pushes/pops.
    /// Wired by `LocalAgentService::spawn_agent_loop` so the
    /// "ECG" stream the desktop renders reflects nested
    /// delegation depth in real time.
    consciousness_tracker: Option<Arc<crate::consciousness::ConsciousnessTracker>>,
    /// Advanced context compressor (LLM-powered summarization).
    /// When set, replaces the simple character-based compressor in
    /// `auto_compress_if_over_threshold`. Requires an `AuxiliaryClient`
    /// for summary generation — constructed from env vars in
    /// `LocalAgentService::spawn_agent_loop`.
    compression: Option<Arc<tokio::sync::Mutex<crate::compression::ContextCompressor>>>,
    /// Latest compressed context summary from the most recent compaction.
    /// Stored here so it can be included in AgentResult for persistence.
    latest_compressed_summary: std::sync::Mutex<Option<String>>,
    /// 2026-06-26 (A): raw `turns_to_summarize` from the most recent
    /// compression, captured by `ContextCompressor::compress` before
    /// it mutated the conversation. `local_agent_service` reads this
    /// in the same `persist_session_with_summary` transaction that
    /// writes `sessions.compressed_summary`, and forwards it to
    /// `SessionPersistence::archive_compressed_turns` so the
    /// original wording is durable in `compression_archives`.
    latest_compression_archived_turns: std::sync::Mutex<Option<Vec<Message>>>,
    /// 2026-06-26 (B): metrics from the most recent compression
    /// (original/compressed turns + tokens + savings). Forwarded
    /// alongside the archived turns so `save_session_atomic` can
    /// write `compression_summary_history` in the same transaction.
    latest_compression_metrics: std::sync::Mutex<Option<crate::compression::CompressionMetrics>>,
    /// Most recent archive refs produced by
    /// [`archive_context_section`]. Surfaced to the frontend so the
    /// user can open the archive file at a specific node anchor.
    /// See `crates/hermix-agent/src/archive.rs` (v2: 2026-06-21).
    last_archive_refs: std::sync::Mutex<Vec<ArchiveRef>>,
    /// Phase 18 (memory recall gating): `false` until the first full
    /// memory recall completes. Once `true`, subsequent `memory_prefetch`
    /// calls use a smaller limit to avoid re-injecting the same memory
    /// every turn — the LLM already has it in the active context window.
    memory_recall_seeded: std::sync::atomic::AtomicBool,
    /// Phase 18 (prompt caching): cached stable portion of the system
    /// prompt (personality, formatting, tool-selection rules). Rebuilt
    /// only when the model or personality config changes. `None` means
    /// uninitialised.
    cached_stable_prompt_key: std::sync::Mutex<Option<(u64, String)>>,
    /// Optional embedding provider for vector recall in memory prefetch.
    /// When set, `memory_prefetch` generates embeddings and passes them
    /// to `MemoryQuery` for cosine similarity search.
    embedding_provider: Option<Arc<dyn hermix_intelligence::embedding::EmbeddingProvider>>,
}

#[derive(Debug, Clone)]
pub(crate) struct TurnRuntimeRoute {
    pub(crate) model: String,
    pub(crate) provider: Option<String>,
    pub(crate) base_url: Option<String>,
    pub(crate) api_key_env: Option<String>,
    pub(crate) api_mode: Option<ApiMode>,
    pub(crate) command: Option<String>,
    pub(crate) args: Vec<String>,
    pub(crate) credential_pool: Option<Arc<CredentialPool>>,
    /// When true (default), merge with [`AgentLoop::primary_credential_pool`] if route pool is unset.
    pub(crate) credential_pool_fallback: bool,
    pub(crate) route_label: Option<String>,
    pub(crate) routing_reason: Option<String>,
    pub(crate) signature: TurnRouteSignature,
}

/// Output of [`AgentLoop::prepare_run_context`]: everything needed to enter
/// the model turn loop without re-running the setup phase.
struct PreparedRun {
    ctx: ContextManager,
    /// Tools the turn loop will pass to the model on each call.
    tool_schemas: Vec<ToolSchema>,
    /// Whether OnSessionStart hooks already fired during setup. If true, the
    /// turn loop must NOT re-fire them (otherwise: double-fire).
    session_started_hooks_fired: bool,
    /// Index of the last user message in `ctx`, if `persist_user_message` is
    /// configured. Used at the end to write it back to the session store.
    persist_user_idx: Option<usize>,
    /// Counter for Codex-style "ack then continue" handling across turns.
    codex_ack_continuations: u32,
    /// Counter for autonomous mode nudges (non-streaming path).
    autonomous_nudge_count: u32,
    /// Whether the agent should review memory at the end of this run (set if
    /// `memory_nudge_interval` reached its threshold during setup).
    review_memory_at_end: bool,
    /// Text of the most-recent user message (the original task). Used by
    /// Codex-style "looks_like_codex_intermediate_ack" and memory review.
    task_hint: String,
}

/// Encapsulates the 3 tracking fields the context-pressure warning
/// logic uses. Lifted out of [`LoopState`] so the cooldown/throttle
/// rules live in one place (and can be unit-tested without spinning
/// up an `AgentLoop`).
///
/// Phase 5 (2026-07-03): replaces 3 fields (`context_pressure_warned_at`,
/// `context_pressure_last_warn_at`, `context_pressure_last_warn_percent`)
/// scattered across `LoopState`. The shared [`should_emit_warning`]
/// method now owns the cooldown + tier-upgrade + percent-step rules.
///
/// Physically split to `context_pressure.rs` (2026-07-04); re-exported
/// here so existing `state.context_pressure.*` call paths compile unchanged.
pub use crate::context_pressure::ContextPressureMonitor;

/// Phase 5 (2026-07-03): cost-guard bookkeeping consolidated into one
/// struct. The four sibling fields (`accumulated_usage`,
/// `session_cost_usd`, `cost_warned`, `forced_runtime_route`) used to
/// live directly on `LoopState` and were passed one-by-one as 4 sibling
/// `&mut` arguments into [`AgentLoop::process_post_response`]. Folding
/// them into one struct:
/// - shrinks `LoopState` from 14 to 10 fields,
/// - shrinks `process_post_response` from 7 to 3 mutable args (3 scalars
///   stay as primitives; the bookkeeping is now `&mut CostGuard`),
/// - keeps the 4-field atomic-mutation at the cost-guard code path
///   where it belongs (a single helper can read + write the state),
/// - lets `publish_from` drop another 4 args.
///
/// The 4 fields stay co-located on `self.cost_guard` and mutate in
/// place via the `record_usage` / `apply_route_downgrade` methods.
/// Reset / `Default` produce the same all-zeros state as before, so
/// `LoopState::default()` semantics are unchanged.
///
/// Physically split to `cost_guard.rs` (2026-07-04); re-exported here
/// so existing `state.cost_guard.*` call paths compile unchanged.
/// `TurnRuntimeRoute` was lifted to `pub(crate)` so the new module
/// can name it; the field visibility stays `pub(crate)` so the
/// runtime publish path (`state.cost_guard.accumulated_usage`) keeps
/// working without accessor-method churn.
pub use crate::cost_guard::{CostGuard, CostGuardAction};
pub(crate) use crate::cost_guard::WarnKind;

/// Phase 5 (2026-07-03): per-loop retry counters consolidated into one
/// struct so `LoopState` doesn't have to carry 3 sibling `u32` fields and
/// `publish_from` doesn't have to take 3 sibling `u32` arguments.
///
/// All three counters follow the same shape:
/// - `bump_*` saturating-increments and returns the new value.
/// - `reset_*` clears the counter (called on the success path).
/// - The `< max` / `>= max` comparison stays at the call site because
///   the action on max differs between the three (finalize vs.
///   continue-with-error vs. fall-through) — folding that decision
///   into the helper would force one path on all three.
///
/// Physically split to `retry_counters.rs` (2026-07-04); re-exported
/// here so existing `state.retry_counters.*` call paths compile
/// unchanged.
pub use crate::retry_counters::RetryCounters;

/// Phase 5 (2026-07-04 follow-up): per-run LLM-result caches consolidated
/// into one struct. The two sibling `Option` fields
/// (`last_checkpoint_messages`, `last_content_with_tools`) used to live
/// directly on `LoopState` and were bridged back from `run()`'s
/// loop-locals via 2 sibling arguments in `publish_from`. Folding them
/// into one struct:
/// - shrinks `LoopState` by 2 fields (8 → 6),
/// - shrinks `publish_from` from 3 args to 1 arg (only `total_turns`
///   remains bridged),
/// - lets `run()` mutate the caches in place via
///   `store_checkpoint` / `store_content_with_tools` /
///   `take_content_with_tools` instead of accumulating to a local
///   alias and re-publishing each iteration,
/// - collapses 2 helper-arg slots (`&mut Option<String>` for
///   `last_content_with_tools`) to 1 (`&mut MessageCache`).
///
/// The 2 fields stay co-located on `self.message_cache` and mutate
/// in place. Reset / `Default` produce the same all-`None` state as
/// before, so `LoopState::default()` semantics are unchanged.
///
/// Physically split to `message_cache.rs` (2026-07-04); re-exported
/// here so existing `state.message_cache.*` call paths compile
/// unchanged.
pub use crate::message_cache::MessageCache;

/// Phase 5 (2026-07-04 follow-up): turn-level timing bookkeeping consolidated
/// into one struct. The two sibling `u64` fields (`total_api_time_ms`,
/// `total_tool_time_ms`) used to live directly on `LoopState` and were
/// bridged back from `run()`'s loop-locals via 2 sibling arguments in
/// `publish_from`. Folding them into one struct:
/// - shrinks `LoopState` by 2 fields (10 → 8),
/// - shrinks `publish_from` from 5 args to 3 args,
/// - lets `run()` mutate the timings in place via `record_api` /
///   `record_tool` instead of accumulating to a local `_total_*` and
///   re-publishing each iteration (the local accumulation was the
///   pre-extraction pattern that the Phase 1.1 bridge set up for
///   `total_turns`; timings don't need that bridge because nothing
///   reads `state.timings` from outside this file — they're just
///   collected for future inspection / metrics).
///
/// The 2 fields stay co-located on `self.timings` and mutate in
/// place. Reset / `Default` produce the same all-zeros state as
/// before, so `LoopState::default()` semantics are unchanged.
///
/// Physically split to `turn_timings.rs` (2026-07-04); re-exported
/// here so existing `state.timings.*` call paths compile unchanged.
pub use crate::turn_timings::TurnTimings;

/// All mutable loop-local state shared between `run()` and `run_stream()`.
/// Centralized to avoid declaring 17+ individual locals in both methods
/// (reduces duplication and eliminates the "forget to sync" bug).
///
/// Phase 1 (2026-07-03) introduces the struct. Phase 1.1 wires the
/// 14 run() + 17 run_stream() locals through it via `publish_from`.
/// Phase 5 (2026-07-03 + 2026-07-04) folds 9 inline fields into 4
/// manager structs ([`TurnTimings`], [`CostGuard`], [`MessageCache`],
/// [`RetryCounters`], [`ContextPressureMonitor`]). After Phase 5
/// (commits a37bba3, d31f7e8) the `publish_from` bridge is gone —
/// `run()` / `run_stream()` write `state.total_turns = total_turns;`
/// directly; the inner manager structs are mutated in place via their
/// own `record_*` / `bump_*` / `store_*` methods.
#[derive(Default)]
struct LoopState {
    /// Total turns completed so far in this run.
    total_turns: u32,
    /// Per-run API + tool wall-clock accumulators. Phase 5 (2026-07-04):
    /// extracted from 2 inline `u64` fields to a [`TurnTimings`] struct
    /// so `publish_from` shrinks by 2 args and the `record_api` /
    /// `record_tool` methods are the single mutation path.
    timings: TurnTimings,
    /// Cost-guard bookkeeping: cumulative usage, USD spend, warn flag,
    /// forced route. Phase 5 (2026-07-03): 4 inline fields that used to
    /// sit directly on `LoopState` moved into [`CostGuard`] which is
    /// mutated in place via `record_usage` / `evaluate_thresholds` /
    /// `install_downgrade`. `publish_from` no longer takes 4 sibling
    /// arguments for these.
    cost_guard: CostGuard,
    /// Per-run message caches (last checkpoint snapshot + last
    /// assistant content with tool calls). Phase 5 (2026-07-04): the 2
    /// sibling `Option` fields that used to live here moved into
    /// [`MessageCache`] which is mutated in place via `store_*` /
    /// `take_*` / `*()` accessors. `publish_from` no longer takes 2
    /// sibling arguments for these.
    message_cache: MessageCache,
    /// Retries against invalid tool names / invalid tool JSON / truncated
    /// tool calls. Phase 5 (2026-07-03): the 3 sibling `u32` fields that
    /// used to live here moved into [`RetryCounters`] and are mutated
    /// in place via `bump_*` / `reset_*` methods. `publish_from` no
    /// longer takes 3 sibling arguments for these.
    retry_counters: RetryCounters,
    /// Context-pressure warning state (tier + cooldown timestamp + last %).
    /// Phase 5 (2026-07-03): extracted from 3 inline fields to consolidate
    /// the throttle/cooldown rules in [`ContextPressureMonitor::should_emit_warning`].
    context_pressure: ContextPressureMonitor,
}

/// Phase 1.3a (2026-07-03): outcome of [`AgentLoop::process_post_response`].
/// The shared post-LLM helper decides between two paths:
/// - `Finalize`: no tool calls → no work left, return the AgentResult.
/// - `Continue`: tool calls present → fall through to tool dispatch.
#[derive(Debug, PartialEq, Eq)]
enum PostResponseAction {
    /// No tool calls: stop the loop. The caller should `return`
    /// `Ok(self.finalize_agent_result(...))`.
    Finalize,
    /// Cost guard tripped (session spend exceeded `max_cost_usd`).
    /// Caller should `return` with `finalize_agent_result(.., false, false, None)`.
    CostGuardTripped,
    /// Tool calls present: caller falls through to `execute_tool_calls`.
    Continue,
}

// ---------------------------------------------------------------------------
// Backwards-compat re-exports (P2.2.4, 2026-06-30)
// ---------------------------------------------------------------------------

impl AgentLoop {
    /// Populate [`AgentConfig::stored_system_prompt`] from SQLite (`sessions.system_prompt`) for session continuation.
    ///
    /// Call before [`AgentLoop::run`] when resuming a gateway/CLI session so Anthropic prefix cache matches Python.
    pub fn hydrate_stored_system_prompt_from_hermes_home(
        config: &mut AgentConfig,
        hermix_home: &std::path::Path,
    ) -> Result<(), AgentError> {
        let Some(ref sid) = config.session_id else {
            return Ok(());
        };
        if sid.trim().is_empty() {
            return Ok(());
        }
        let sp = crate::session_persistence::SessionPersistence::new(hermix_home);
        sp.ensure_db()?;
        if let Some(prompt) = sp.get_system_prompt(sid)? {
            config.stored_system_prompt = Some(prompt);
        }
        Ok(())
    }

    /// Build [`AgentResult`] messages for return / persistence (applies `persist_user_message` override).
    fn messages_for_persisted_result(
        &self,
        ctx: &ContextManager,
        persist_user_idx: Option<usize>,
    ) -> Vec<Message> {
        let mut msgs = ctx.get_messages().to_vec();
        if let (Some(idx), Some(override_text)) = (
            persist_user_idx,
            self.config.persist_user_message.as_deref(),
        ) {
            if let Some(msg) = msgs.get_mut(idx) {
                if msg.role == MessageRole::User {
                    msg.content = Some(override_text.to_string());
                }
            }
        }
        msgs
    }

    fn graceful_interrupt_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
    ) -> AgentResult {
        self.finalize_agent_result(
            ctx,
            total_turns,
            tool_errors,
            accumulated_usage,
            session_cost_usd,
            session_started_hooks_fired,
            persist_user_idx,
            false,
            true,
            None,
        )
    }

    fn should_emit_session_reset_hook(messages: &[Message]) -> bool {
        messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)
            .and_then(|m| m.content.as_deref())
            .map(|content| {
                let trimmed = content.trim();
                matches!(trimmed, "/reset" | "/new")
            })
            .unwrap_or(false)
    }

    #[allow(clippy::too_many_arguments)]
    fn finalize_agent_result(
        &self,
        ctx: &ContextManager,
        total_turns: u32,
        tool_errors: &[hermix_core::ToolErrorRecord],
        accumulated_usage: Option<UsageStats>,
        session_cost_usd: f64,
        session_started_hooks_fired: bool,
        persist_user_idx: Option<usize>,
        finished_naturally: bool,
        interrupted: bool,
        finish_reason: Option<String>,
    ) -> AgentResult {
        let messages = ctx.get_messages();
        let end_ctx = serde_json::json!({
            "session_id": self.config.session_id,
            "turns": total_turns,
            "finished_naturally": finished_naturally,
            "interrupted": interrupted,
            "session_started_hooks_fired": session_started_hooks_fired,
        });
        let _ = self.invoke_hook(HookType::OnSessionEnd, &end_ctx);
        if Self::should_emit_session_reset_hook(messages) {
            let reset_ctx = serde_json::json!({
                "session_id": self.config.session_id,
                "turns": total_turns,
                "source": "user_command",
            });
            let _ = self.invoke_hook(HookType::OnSessionReset, &reset_ctx);
        }
        self.memory_on_session_end(messages);
        let finalize_ctx = serde_json::json!({
            "session_id": self.config.session_id,
            "turns": total_turns,
            "tool_errors": tool_errors.len(),
            "session_cost_usd": session_cost_usd,
        });
        let _ = self.invoke_hook(HookType::OnSessionFinalize, &finalize_ctx);

        // P1-3 (2026-06-27): persist the final accumulated token usage
        // to `token_usage` so `/insights` can chart real numbers. We
        // do this *after* the session-end hooks fire (so a hook can
        // still mutate usage if needed) and *before* we build the
        // `AgentResult` (so callers see the same numbers in
        // `result.usage` that we just wrote to disk).
        //
        // Best-effort: a failed insert never breaks the response. The
        // `/insights` panel just shows "no usage data" instead of an
        // HTTP error. This matches the existing pattern in
        // `save_session_atomic` where the DB write failure is logged
        // but the function still returns Ok.
        if let (Some(ref sid), Some(usage)) =
            (self.config.session_id.as_ref(), accumulated_usage.as_ref())
        {
            if !sid.trim().is_empty()
                && (usage.prompt_tokens > 0 || usage.completion_tokens > 0)
            {
                let home_buf = self
                    .config
                    .hermix_home
                    .as_ref()
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(hermix_config::paths::hermix_home);
                let sp = crate::session_persistence::SessionPersistence::new(&home_buf);
                if let Err(e) = sp.persist_token_usage(sid, &self.config.model, usage) {
                    tracing::warn!(
                        session_id = %sid,
                        error = %e,
                        "P1-3: failed to persist token_usage (non-fatal); /insights will show stale data"
                    );
                }
            }
        }

        AgentResult {
            messages: self.messages_for_persisted_result(ctx, persist_user_idx),
            finished_naturally,
            total_turns,
            tool_errors: tool_errors.to_vec(),
            usage: accumulated_usage,
            interrupted,
            session_cost_usd: Some(session_cost_usd),
            session_started_hooks_fired,
            finish_reason,
            compressed_summary: self.latest_compressed_summary.lock().ok().and_then(|g| g.clone()),
            compression_archived_turns: self
                .latest_compression_archived_turns
                .lock()
                .ok()
                .and_then(|g| g.clone()),
            compression_metrics: self
                .latest_compression_metrics
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|m| m.to_snapshot())),
        }
    }

    /// Phase 1.3a (2026-07-03): the post-LLM logic that was duplicated
    /// ~200 lines in `run()` and `run_stream()`. Fires the PostLlmCall
    /// hook, accumulates usage, runs the cost guard (with auto-degrade
    /// to the configured cheap model), and decides between
    /// `Finalize` (no tool calls → caller returns) and `Continue`
    /// (tool calls → caller falls through to `execute_tool_calls`).
    ///
    /// `stream_mode` is `true` for `run_stream()` — the helper uses
    /// this to emit a stream-specific `tracing` line and to forward
    /// the `finish_reason` through the finalize path.
    ///
    /// Phase 5 (2026-07-03): 7 mutable scalar args collapsed to
    /// `&mut CostGuard` + `&mut Option<String>` (4 args shrunk to 1
    /// struct). `record_usage` / `evaluate_thresholds` / `install_downgrade`
    /// on [`CostGuard`] absorb the 4-field atomic-mutation, so the
    /// helper signature dropped from 11 args to 7. The two remaining
    /// mutable scalars — `last_content_with_tools` — stay on the
    /// helper signature because they're stored on `LoopState` (used by
    /// the codex-ack continuation logic on the next turn) and not on
    /// `CostGuard`.
    ///
    /// Phase 5 (2026-07-04 follow-up): `&mut Option<String>` for
    /// `last_content_with_tools` collapsed to `&mut MessageCache`,
    /// dropping the helper to 6 mutable args (the `Option<String>` is
    /// now reached via `cache.store_content_with_tools(...)` inside
    /// the helper). The companion helper `prepare_assistant_turn`
    /// also takes `&mut MessageCache` for the same reason.
    #[allow(clippy::too_many_arguments)]
    fn process_post_response(
        &self,
        ctx: &mut ContextManager,
        response: &hermix_core::LlmResponse,
        turn_usage_acc: Option<&UsageStats>,
        total_turns: u32,
        api_elapsed: u64,
        cost_guard: &mut CostGuard,
        message_cache: &mut MessageCache,
        stream_mode: bool,
    ) -> Result<PostResponseAction, AgentError> {
        let has_tool_calls = response
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|tc| !tc.is_empty());

        if stream_mode {
            tracing::info!(
                turn = total_turns,
                has_tool_calls,
                finish_reason = response.finish_reason.as_deref().unwrap_or(""),
                "llm_response_received"
            );
        }

        // --- Post-LLM hook ---
        let post_ctx = serde_json::json!({
            "turn": total_turns,
            "api_time_ms": api_elapsed,
            "has_tool_calls": has_tool_calls,
        });
        let post_results = self.invoke_hook(HookType::PostLlmCall, &post_ctx);
        self.inject_hook_context(&post_results, ctx);

        cost_guard.record_usage(turn_usage_acc, response.model.as_str(), &self.config);

        // --- Cost guard ---
        let action = cost_guard.evaluate_thresholds(
            self.config.max_cost_usd,
            self.config.cost_guard_degrade_at_ratio,
            || self.resolve_cost_degrade_model(),
        );
        // Two independent side effects: warn (system message, route
        // downgrade) and trip (system message, terminate loop). Both
        // can fire on the same call when spend jumped past both
        // thresholds in one turn — fire warn first, then trip, so the
        // user sees "switching to cheaper model" before the
        // terminating "stopping loop" message.
        if let Some(warn) = action.warn {
            match warn.kind {
                WarnKind::Downgrade { model } => {
                    let route = self.turn_route_cost_guard(model.clone());
                    cost_guard.install_downgrade(route);
                    ctx.add_message(Message::system(format!(
                        "Cost guard: session spend is now ${:.4}/${:.4}. Switching to cheaper model `{}`.",
                        warn.session_cost_usd, warn.limit, model
                    )));
                }
                WarnKind::Warn => {
                    ctx.add_message(Message::system(format!(
                        "Cost guard warning: session spend is now ${:.4}/${:.4}.",
                        warn.session_cost_usd, warn.limit
                    )));
                }
            }
        }
        if let Some(trip) = action.trip {
            ctx.add_message(Message::system(format!(
                "Cost guard tripped: session spend ${:.4} exceeded max_cost_usd ${:.4}. Stopping loop.",
                trip.session_cost_usd, trip.limit
            )));
            return Ok(PostResponseAction::CostGuardTripped);
        }

        let tool_calls_present = response
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|tc| !tc.is_empty());

        if !tool_calls_present {
            // No tool calls — agent is done. Save the last assistant content
            // for ack-continuation tracking.
            if response
                .message
                .content
                .as_deref()
                .is_some_and(|s| !s.is_empty())
            {
                message_cache.store_content_with_tools(
                    response
                        .message
                        .content
                        .as_deref()
                        .map(strip_think_blocks_for_ack)
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty()),
                );
            }
            return Ok(PostResponseAction::Finalize);
        }

        // Tool calls present — caller continues to dispatch.
        Ok(PostResponseAction::Continue)
    }

    /// Phase 2.2 (2026-07-03): shared per-turn post-LLM setup that
    /// `run()` and `run_stream()` previously open-coded after the
    /// `process_post_response` call. Builds the local `assistant_msg`,
    /// appends it to the context, updates `last_content_with_tools` if
    /// the assistant content came bundled with tool calls (used by the
    /// codex-ack continuation logic on the next turn), and reports
    /// whether any prior tool result exists in the history.
    ///
    /// Returns the three locals the caller needs:
    /// - `history_includes_tool`: did any earlier turn leave a `Tool`
    ///   message in the context? Used by
    ///   `decide_post_response_fallback` to know whether tool
    ///   execution is part of the protocol context.
    /// - `assistant_msg`: the LLM's response, freshly appended to
    ///   the context.
    /// - `tool_calls`: the LLM's claimed tool calls, cloned for the
    ///   caller to dispatch / dedup.
    #[allow(clippy::type_complexity)]
    fn prepare_assistant_turn(
        &self,
        ctx: &mut ContextManager,
        response: &hermix_core::LlmResponse,
        message_cache: &mut MessageCache,
    ) -> (bool, Message, Option<Vec<ToolCall>>) {
        let history_includes_tool = ctx
            .get_messages()
            .iter()
            .any(|m| m.role == MessageRole::Tool);
        let assistant_msg = response.message.clone();
        let tool_calls = assistant_msg.tool_calls.clone();
        ctx.add_message(assistant_msg.clone());
        if assistant_msg
            .tool_calls
            .as_ref()
            .is_some_and(|v| !v.is_empty())
            && Self::assistant_visible_text_after_think_blocks(&assistant_msg)
        {
            message_cache.store_content_with_tools(
                assistant_msg
                    .content
                    .as_deref()
                    .map(strip_think_blocks_for_ack)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            );
        }
        (history_includes_tool, assistant_msg, tool_calls)
    }

    /// Phase 2.3 (2026-07-03): centralised `mute_post_response`
    /// control chunk emit. The same AtomicBool-swap-then-conditional-
    /// emit pattern was open-coded in two places inside `run_stream()`:
    /// the housekeeping block after tool dispatch (line ~5980) and the
    /// terminal finalize path (line ~5936). This helper takes the
    /// stream callback, the mute flag, and the desired next state, and
    /// only fires the control chunk when the state actually changes
    /// (the frontend only needs to know about transitions, not every
    /// dispatch). Both call sites become a single 1-line call.
    fn emit_mute_transition(
        on_chunk: &Box<dyn Fn(StreamChunk) + Send + Sync>,
        stream_mute: &std::sync::atomic::AtomicBool,
        next_muted: bool,
    ) {
        use std::sync::atomic::Ordering;
        let was_muted = stream_mute.swap(next_muted, Ordering::AcqRel);
        if was_muted != next_muted {
            on_chunk(StreamChunk {
                delta: Some(hermix_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: Some(serde_json::json!({
                        "control": "mute_post_response",
                        "enabled": next_muted
                    })),
                }),
                finish_reason: None,
                usage: None,
            });
        }
    }

    /// Phase 2.1 (2026-07-03): apply the four-variant
    /// `PostResponseFallback` outcome produced by
    /// `crate::post_response_fallback::decide_post_response_fallback`.
    /// Both `run()` and `run_stream()` previously open-coded this
    /// match arm — the only difference between the two call sites
    /// was the tracing message text. This helper centralises the
    /// match and parameterises the stream-mode log suffix.
    ///
    /// Returns `true` when a `ContinueWithUserMsg` or
    /// `ContinueWithSystemNudge` variant injected a message into the
    /// context — the caller should `continue;` to restart the loop
    /// with the injected message. Returns `false` when the variant
    /// was `PatchLastAssistant` (which mutated the last assistant
    /// message in place) or `Finalize` (a no-op) — the caller should
    /// fall through to its terminal finalize path.
    fn apply_fallback_outcome(
        &self,
        ctx: &mut ContextManager,
        action: crate::post_response_fallback::PostResponseFallback,
        stream_mode: bool,
        total_turns: u32,
        autonomous_nudge_count: u32,
    ) -> bool {
        use crate::post_response_fallback::PostResponseFallback;
        match action {
            PostResponseFallback::ContinueWithUserMsg(msg) => {
                ctx.add_message(Message::user(&msg));
                true
            }
            PostResponseFallback::ContinueWithSystemNudge(msg) => {
                tracing::debug!(
                    nudge_count = autonomous_nudge_count,
                    total_turns,
                    "autonomous_mode{}: nudging LLM back into action",
                    if stream_mode { "" } else { " [non-stream]" }
                );
                ctx.add_message(Message::system(&msg));
                true
            }
            PostResponseFallback::PatchLastAssistant { content } => {
                if let Some(last) = ctx.get_messages_mut().last_mut() {
                    if last.role == MessageRole::Assistant {
                        last.content = Some(content);
                    }
                }
                false
            }
            PostResponseFallback::Finalize => false,
        }
    }

    /// v2 (2026-06-21): most recent archive refs produced by
    /// `auto_compress_if_over_threshold`. Empty when the context has
    /// never crossed the 80% pressure threshold. Frontend uses this
    /// to render the "see archived context" affordance.
    pub fn last_archive_refs(&self) -> Vec<ArchiveRef> {
        self.last_archive_refs
            .lock()
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default()
    }

    /// Create a new agent loop.
    pub fn new(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
    ) -> Self {
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt: InterruptController::new(),
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh: OAuthRefresh::default(),
            sub_agent_orchestrator: None,
            pending_steer: Arc::new(Mutex::new(None)),
            tool_router: None,
            memory_router: None,
            always_include: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "memory".to_string(),
            ],
            prefetcher: None,
            learned_prefetch_stats: Arc::new(Mutex::new(
                hermix_intelligence::prefetch::IntentStats::default(),
            )),
            focus_stack: Arc::new(Mutex::new(FocusStack::new())),
            consciousness_tracker: None,
            compression: None,
            latest_compressed_summary: std::sync::Mutex::new(None),
            latest_compression_archived_turns: std::sync::Mutex::new(None),
            latest_compression_metrics: std::sync::Mutex::new(None),
            last_archive_refs: std::sync::Mutex::new(Vec::new()),
            memory_recall_seeded: std::sync::atomic::AtomicBool::new(false),
            cached_stable_prompt_key: std::sync::Mutex::new(None),
            embedding_provider: None,
        }
    }

    /// Create a new agent loop with a shared interrupt controller.
    pub fn with_interrupt(
        config: AgentConfig,
        tool_registry: Arc<ToolRegistry>,
        llm_provider: Arc<dyn LlmProvider>,
        interrupt: InterruptController,
    ) -> Self {
        Self {
            config,
            tool_registry,
            llm_provider,
            interrupt,
            plugin_manager: None,
            callbacks: Arc::new(AgentCallbacks::default()),
            delegate_depth: 0,
            primary_credential_pool: None,
            evolution_counters: Arc::new(Mutex::new(EvolutionCounters::default())),
            oauth_refresh: OAuthRefresh::default(),
            sub_agent_orchestrator: None,
            pending_steer: Arc::new(Mutex::new(None)),
            tool_router: None,
            memory_router: None,
            always_include: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "memory".to_string(),
            ],
            prefetcher: None,
            learned_prefetch_stats: Arc::new(Mutex::new(
                hermix_intelligence::prefetch::IntentStats::default(),
            )),
            focus_stack: Arc::new(Mutex::new(FocusStack::new())),
            consciousness_tracker: None,
            compression: None,
            latest_compressed_summary: std::sync::Mutex::new(None),
            latest_compression_archived_turns: std::sync::Mutex::new(None),
            latest_compression_metrics: std::sync::Mutex::new(None),
            last_archive_refs: std::sync::Mutex::new(Vec::new()),
            memory_recall_seeded: std::sync::atomic::AtomicBool::new(false),
            cached_stable_prompt_key: std::sync::Mutex::new(None),
            embedding_provider: None,
        }
    }

    /// Attach a memory router for Phase 15 time-aware recall.
    pub fn with_memory_router(mut self, router: Arc<hermix_focus::MemoryRouter>) -> Self {
        self.memory_router = Some(router);
        self
    }

    /// Attach an embedding provider for vector recall in memory prefetch.
    pub fn with_embedding_provider(
        mut self,
        provider: Arc<dyn hermix_intelligence::embedding::EmbeddingProvider>,
    ) -> Self {
        self.embedding_provider = Some(provider);
        self
    }

    /// Attach an in-process sub-agent orchestrator. When set, `delegate_task`
    /// tool calls are actually executed by the orchestrator instead of just
    /// returning a signal envelope. See
    /// [`crate::sub_agent_orchestrator::SubAgentOrchestrator`].
    pub fn with_sub_agent_orchestrator(
        mut self,
        orchestrator: Arc<crate::sub_agent_orchestrator::SubAgentOrchestrator>,
    ) -> Self {
        self.sub_agent_orchestrator = Some(orchestrator);
        self
    }

    /// M1: replace the per-loop `FocusStack<SessionFrame>`. Default
    /// is an empty stack (constructed in `new` / `with_interrupt`).
    /// Tests use this to inject a pre-populated stack; production
    /// callers should leave the default.
    pub fn with_focus_stack(mut self, stack: Arc<Mutex<FocusStack<SessionFrame>>>) -> Self {
        self.focus_stack = stack;
        self
    }

    /// M1.6.3: attach the consciousness tracker so the focus stack
    /// depth is reflected in the "ECG" stream the desktop renders.
    /// Called from `LocalAgentService::spawn_agent_loop`; tests
    /// typically leave the default (`None`).
    pub fn with_consciousness_tracker(
        mut self,
        tracker: Arc<crate::consciousness::ConsciousnessTracker>,
    ) -> Self {
        self.consciousness_tracker = Some(tracker);
        self
    }

    /// Attach the primary runtime credential pool (API key rotation).
    pub fn with_primary_credential_pool(mut self, pool: Arc<CredentialPool>) -> Self {
        self.primary_credential_pool = Some(pool);
        self
    }

    /// Set the plugin manager.
    pub fn with_plugins(mut self, pm: Arc<std::sync::Mutex<PluginManager>>) -> Self {
        self.plugin_manager = Some(pm);
        self
    }

    /// Set the callbacks.
    pub fn with_callbacks(mut self, cb: AgentCallbacks) -> Self {
        self.callbacks = Arc::new(cb);
        self
    }

    /// Set the delegate depth.
    pub fn with_delegate_depth(mut self, depth: u32) -> Self {
        self.delegate_depth = depth;
        self
    }

    /// Attach a tool schema router. When set, the LLM only sees the
    /// router's output instead of the full tool registry (Phase 7 H1).
    /// Wrap your router with [`AlwaysInclude`] if you want a fixed
    /// must-have set to bypass the router.
    pub fn with_tool_router(mut self, router: Arc<dyn ToolSchemaRouter>) -> Self {
        self.tool_router = Some(router);
        self
    }

    /// Override the always-include tool whitelist.
    pub fn with_always_include(mut self, names: Vec<String>) -> Self {
        self.always_include = names;
        self
    }

    /// Attach an intent-aware prefetcher (Phase 15 H2).
    /// When set, the agent loop runs the prefetcher before each LLM call,
    /// dispatches Safe tools in parallel, and injects results as a
    /// system-prompt block.
    pub fn with_prefetcher(
        mut self,
        prefetcher: Prefetcher,
        learned_stats: hermix_intelligence::prefetch::IntentStats,
    ) -> Self {
        self.prefetcher = Some(Arc::new(prefetcher));
        self.learned_prefetch_stats = Arc::new(Mutex::new(learned_stats));
        self
    }

    /// Attach an advanced LLM-powered context compressor. When set,
    /// `auto_compress_if_over_threshold` uses this instead of the
    /// simple character-based compressor, producing structured
    /// summaries that preserve goals, progress, decisions, and files.
    pub fn with_compressor(
        mut self,
        compressor: crate::compression::ContextCompressor,
    ) -> Self {
        self.compression = Some(Arc::new(tokio::sync::Mutex::new(compressor)));
        self
    }

    // -- M5 ACUI emit ------------------------------------------------------

    /// Emit an ACUI directive to upstream listeners (if any).
    ///
    /// No-op if no `on_acui_directive` callback was wired (e.g. unit
    /// tests, batched runs). The directive is JSON-serialized and passed
    /// to the callback as a string; the upstream layer is responsible
    /// for deserializing and forwarding it as a `StreamEvent::Acui`.
    ///
    /// This method never blocks and never returns an error — the ACUI
    /// pipeline is best-effort. A failure in the callback is logged
    /// and swallowed (the agent keeps running).
    pub fn emit_acui(&self, directive: AcuiDirective) {
        let Some(ref cb) = self.callbacks.on_acui_directive else {
            return;
        };
        match serde_json::to_string(&directive) {
            Ok(payload) => cb(&payload),
            Err(e) => {
                tracing::warn!(
                    target: "hermix.agent",
                    error = %e,
                    "failed to serialize AcuiDirective, dropping emit"
                );
            }
        }
    }

    /// Convenience: emit a `Show` directive.
    pub fn emit_acui_show(
        &self,
        id: impl Into<String>,
        kind: impl Into<String>,
        props: serde_json::Value,
    ) {
        self.emit_acui(AcuiDirective::Show(hermix_core::AcuiShow::new(
            id, kind, props,
        )));
    }

    /// Convenience: emit an `Update` directive.
    pub fn emit_acui_update(&self, id: impl Into<String>, patch: serde_json::Value) {
        self.emit_acui(AcuiDirective::Update(hermix_core::AcuiUpdate::new(
            id, patch,
        )));
    }

    /// Convenience: emit a `Hide` directive.
    pub fn emit_acui_hide(&self, id: impl Into<String>) {
        self.emit_acui(AcuiDirective::Hide(hermix_core::AcuiHide::new(id)));
    }

    /// Build a default router (KeywordRouter + AlwaysInclude).
    /// Convenience for callers who want the Phase 7 H1 default without
    /// hand-rolling the wiring.
    pub fn default_tool_router() -> Arc<dyn ToolSchemaRouter> {
        let kw = Arc::new(KeywordRouter::new());
        Arc::new(AlwaysInclude::new(
            kw,
            vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "terminal".to_string(),
                "memory".to_string(),
                // read_pdf and list_files must be always-forwarded so the
                // LLM does not fall back to "guess the path via terminal"
                // when the user supplies an explicit file path (e.g.
                // "根据 G:\…\112号…通知.pdf 起草通知").  Previously the
                // KeywordRouter scored read_pdf ≈ 0 on CJK-heavy queries,
                // causing the agent to waste turns on terminal cd/ls
                // instead of calling the tool directly.
                "read_pdf".to_string(),
                "list_files".to_string(),
                // Browser and computer-use tools must be always-forwarded
                // so the LLM can access them when the user explicitly
                // requests web browsing, page interaction, or GUI
                // automation. Without this, KeywordRouter scores them ≈ 0
                // on CJK-heavy queries and the LLM never sees them.
                "browser_navigate".to_string(),
                "browser_snapshot".to_string(),
                "browser_click".to_string(),
                "browser_type".to_string(),
                "browser_scroll".to_string(),
                "computer_use".to_string(),
            ],
        ))
    }

    /// Resolve the tool schemas to send to the LLM this turn.
    ///
    /// Behavior:
    /// - If `tool_router` is `None`: return `candidates` unchanged.
    /// - If `tool_router` is `Some`: build a [`TurnContext`] from the
    ///   last user message + recent history, call `route`, and return
    ///   the routed schemas. The `always_include` whitelist is enforced
    ///   by the router itself if the caller wrapped it with
    ///   [`AlwaysInclude`]; this method does not re-wrap.
    pub fn route_tools(
        &self,
        candidates: Vec<ToolSchema>,
        messages: &[Message],
    ) -> Vec<ToolSchema> {
        let Some(router) = self.tool_router.as_ref() else {
            return candidates;
        };
        let user_message = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let recent: Vec<Message> = messages
            .iter()
            .rev()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let ctx = TurnContext {
            user_message,
            recent_messages: recent,
            active_toolsets: self.config.active_toolsets.clone(),
            max_tools: candidates.len().min(self.config.max_routed_tools),
        };
        let routed: Vec<ToolSchema> = router
            .route(&ctx, candidates)
            .into_iter()
            .map(|r| r.schema)
            .collect();
        // Progressive disclosure: if deferred tools exceed threshold,
        // replace them with tool_search/tool_describe/tool_call bridges.
        let assembly = hermix_tools::assemble_tool_schemas(routed);
        if assembly.activated {
            tracing::info!(
                deferred = assembly.deferred_count,
                "tool_search: {} deferred tools behind bridge",
                assembly.deferred_count,
            );
        }
        assembly.tool_schemas
    }

    /// Queue mid-run user guidance (Python `AIAgent.steer`).
    ///
    /// Returns `false` when the text is empty/whitespace-only.
    pub fn steer(&self, text: &str) -> bool {
        let Some(next) = normalize_accepted_steer_text(text) else {
            return false;
        };
        if let Ok(mut guard) = self.pending_steer.lock() {
            *guard = Some(concat_pending(guard.take(), next));
            return true;
        }
        false
    }

    /// Clear any queued `/steer` guidance without applying it.
    pub fn clear_pending_steer(&self) {
        if let Ok(mut guard) = self.pending_steer.lock() {
            *guard = None;
        }
    }

    // ── M1: FocusStack plumbing ────────────────────────────────────
    //
    // The agent loop owns a per-session `FocusStack<SessionFrame>`.
    // Tool-dispatch paths can call `note_delegation_start` to push a
    // frame for each `delegate_task` invocation and
    // `note_delegation_end` to pop it on completion. Popped
    // `Cleared` frames generate a `FocusConclusion` that is
    // buffered in `pending_conclusions` until
    // `drain_focus_conclusions_to_memory_router` ships them to the
    // long-term memory backend.

    /// Push a `SessionRoot` frame for the current session. Idempotent
    /// — a second call is a no-op if a root frame is already on the
    /// stack. Callers should invoke this once at `run()` start.
    pub fn note_session_started(&self, task: impl Into<String>) {
        let mut stack = match self.focus_stack.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if stack
            .frames()
            .iter()
            .any(|f| matches!(f.payload.kind, hermix_focus::SessionFrameKind::SessionRoot))
        {
            return;
        }
        stack.push(FocusFrame::new(SessionFrame::root(task)));
    }

    /// Push a `Delegation` frame for a `delegate_task` invocation.
    /// `sub_agent_id` is the UUID the orchestrator / sub-agent
    /// returns; pass an empty string if not yet known (it will be
    /// matched by depth in `note_delegation_end`).
    ///
    /// Returns the frame id, or `None` if the lock is poisoned.
    pub fn note_delegation_start(
        &self,
        task: impl Into<String>,
        sub_agent_id: impl Into<String>,
    ) -> Option<hermix_focus::FocusFrameId> {
        let mut stack = self.focus_stack.lock().ok()?;
        let depth = self.delegate_depth;
        let frame = FocusFrame::new(SessionFrame::delegation(task, sub_agent_id, depth));
        Some(stack.push(frame))
    }

    /// Pop the topmost `Delegation` frame whose payload matches
    /// `sub_agent_id` (or the topmost delegation frame if
    /// `sub_agent_id` is empty). The popped frame is `mark_clear`-ed
    /// first so the next `pop` generates a `FocusConclusion`.
    ///
    /// Returns the depth the popped frame was at, or `None` if no
    /// matching frame was on the stack.
    pub fn note_delegation_end(
        &self,
        sub_agent_id: &str,
        status: SessionFrameStatus,
    ) -> Option<u32> {
        let mut stack = self.focus_stack.lock().ok()?;
        // Find the target frame: topmost Delegation matching
        // sub_agent_id, or just the topmost Delegation if the id
        // is empty.
        let target_idx = {
            let frames = stack.frames();
            let mut found: Option<usize> = None;
            for (i, f) in frames.iter().enumerate().rev() {
                if !matches!(f.payload.kind, hermix_focus::SessionFrameKind::Delegation) {
                    continue;
                }
                if sub_agent_id.is_empty() || f.payload.sub_agent_id == sub_agent_id {
                    found = Some(i);
                    break;
                }
            }
            found?
        };
        let popped_depth = stack.frames()[target_idx].payload.depth;
        // Finish the payload first (status + ended_at), then
        // mark_clear (idempotent) and pop.
        let target_id = stack.frames()[target_idx].id;
        if let Some(frame) = stack.get_mut(target_id) {
            frame.payload.finish(status);
            frame.mark_clear();
        }
        // The pop API only pops the top, so to pop an interior
        // frame we have to drain-and-rebuild. This is cheap
        // because the stack is shallow (one entry per
        // in-flight delegate_task).
        let frames_after_target = stack.frames().len() - target_idx - 1;
        let mut stash: Vec<FocusFrame<SessionFrame>> = Vec::with_capacity(frames_after_target);
        for _ in 0..frames_after_target {
            stash.push(stack.pop().expect("stack non-empty by len arithmetic"));
        }
        // Discard the popped target frame: the relevant data (depth,
        // status, ended_at) was extracted into `popped_depth` and
        // `target_id` above, and `finish()` + `mark_clear()` already
        // ran before the pop.
        let _popped = stack.pop().expect("target frame exists");
        stash.reverse();
        for f in stash {
            stack.push(f);
        }
        Some(popped_depth)
    }

    /// Current depth of the focus stack (number of frames on it).
    /// Returned as `u8` because the consciousness tracker's
    /// `set_focus_depth` takes `u8` and the stack never grows past
    /// `MAX_DELEGATE_DEPTH` (typically 5) in practice.
    pub fn current_focus_depth(&self) -> u8 {
        self.focus_stack
            .lock()
            .map(|s| s.depth() as u8)
            .unwrap_or(0)
    }

    /// Drain pending `FocusConclusion`s from the focus stack and
    /// forward them to the long-term memory backend. Returns the
    /// number of conclusions processed.
    ///
    /// No-op if `memory_router` is `None` (conclusions are still
    /// drained from the focus stack — they don't pile up).
    ///
    /// M1.6.4: this now actually persists via the router's
    /// `upsert_conclusion` fan-out. Backends that don't implement
    /// the write surface (8 HTTPS plugins) return
    /// `MemoryError::NotImplemented` and the router skips them
    /// silently. Only the embedded SQLite backend (and any future
    /// write-capable backend) actually writes to long-term store.
    pub fn drain_focus_conclusions_to_memory_router(&self) -> usize {
        let conclusions = {
            let Ok(mut stack) = self.focus_stack.lock() else {
                return 0;
            };
            stack.drain_conclusions()
        };
        if conclusions.is_empty() {
            return 0;
        }
        let Some(router) = self.memory_router.as_ref() else {
            tracing::debug!(
                target: "hermix.agent",
                count = conclusions.len(),
                "drain_focus_conclusions: no memory_router attached, dropping conclusions"
            );
            return 0;
        };
        let session_id = self.config.session_id.clone().unwrap_or_default();
        let router = router.clone();
        let count = conclusions.len();
        // Phase 17 (BaiLongma compression backfill): deterministic
        // fact extraction. Each conclusion becomes a row in the
        // `facts` table with a canonical_key derived from
        // (session_id, source_frame_id) so re-running is idempotent.
        // This is sync (SQLite is sub-ms) — we do it before spawning
        // the async persistence so the fact row lands in the same
        // SQLite connection window as the conclusion row.
        //
        // We only call backfill on the embedded backend (the only
        // backend that owns the `facts` table). The router-level
        // `upsert_conclusion` already fans out to every backend.
        for c in &conclusions {
            if let Some(embedded) = router
                .find_embedded_backend()
            {
                if let Err(e) = embedded.backfill_compression_fact(&session_id, c) {
                    tracing::warn!(
                        target: "hermix.agent",
                        session_id = %session_id,
                        conclusion_id = %c.source_frame_id,
                        error = %e,
                        "drain_focus_conclusions: backfill_compression_fact failed"
                    );
                }
            }
        }
        // Persist asynchronously — don't block the agent loop on
        // SQLite writes. Errors are logged via the router's
        // tolerate_errors policy (default: log + continue).
        tokio::spawn(async move {
            for c in &conclusions {
                if let Err(e) = router.upsert_conclusion(c).await {
                    tracing::warn!(
                        target: "hermix.agent",
                        session_id = %session_id,
                        conclusion_id = %c.source_frame_id,
                        error = %e,
                        "drain_focus_conclusions: router.upsert_conclusion failed"
                    );
                }
            }
        });
        count
    }

    fn drain_pending_steer(&self) -> Option<String> {
        if let Ok(mut guard) = self.pending_steer.lock() {
            return guard.take();
        }
        None
    }

    fn restash_pending_steer(&self, text: String) {
        if let Ok(mut guard) = self.pending_steer.lock() {
            *guard = Some(text);
        }
    }

    /// Pre-LLM-call drain: inject steer into the last tool result if possible; otherwise restash.
    fn pre_api_apply_pending_steer(&self, ctx: &mut ContextManager) {
        let Some(text) = self.drain_pending_steer() else {
            return;
        };
        let msgs = ctx.get_messages_mut();
        if apply_pending_steer_to_last_tool_message(msgs, &text) {
            return;
        }
        self.restash_pending_steer(text);
    }

    /// Post-tool-call apply: inject steer into the last tool result if pending.
    fn post_tool_apply_pending_steer(&self, ctx: &mut ContextManager) {
        let Some(text) = self.drain_pending_steer() else {
            return;
        };
        let msgs = ctx.get_messages_mut();
        if apply_pending_steer_to_last_tool_message(msgs, &text) {
            return;
        }
        self.restash_pending_steer(text);
    }

    // -- Plugin hook helpers ------------------------------------------------

    fn invoke_hook(&self, hook: HookType, ctx_val: &Value) -> Vec<HookResult> {
        if let Some(ref pm) = self.plugin_manager {
            if let Ok(pm) = pm.lock() {
                return pm.invoke_hook(hook, ctx_val);
            }
        }
        Vec::new()
    }

    fn inject_hook_context(&self, results: &[HookResult], ctx: &mut ContextManager) {
        for r in results {
            if let HookResult::InjectContext(text) = r {
                ctx.add_message(Message::system(text));
            }
        }
    }

    // -- Memory helpers ----------------------------------------------------

    async fn memory_prefetch(&self, query: &str, session_id: &str) -> String {
        if self.config.skip_memory {
            return String::new();
        }

        // M2 (2026-07-03): recall gating + budget pulled from config.
        // Defaults are 50 first / 5 subsequent recall, 4000 char block
        // cap, 0.30 salience floor. Tunables were previously hardcoded.
        let is_seeded = self.memory_recall_seeded.load(std::sync::atomic::Ordering::Relaxed);
        let limit = if is_seeded {
            self.config.memory_recall_limit_subsequent
        } else {
            self.config.memory_recall_limit_first
        };
        let char_budget = self.config.memory_char_budget;
        let min_salience = self.config.memory_min_salience;

        // --- Phase 15: Time-aware recall via MemoryLayer path ----------
        let time_window = extract_time_window(query);

        let recall_start = std::time::Instant::now();
        let structured_hits: Vec<MemoryHit> = if let Some(ref router) = self.memory_router {
            let mut q = MemoryQuery::recall(query)
                .with_limit(limit)
                .with_session_id(session_id.to_string());
            if let Some(tw) = time_window {
                q = q.with_time_window(tw);
            }
            // Generate embedding for vector recall if provider is available
            if let Some(ref ep) = self.embedding_provider {
                match ep.embed(query).await {
                    Ok(embedding) => { q.embedding = Some(embedding); }
                    Err(e) => tracing::debug!("embedding failed (falling back to text recall): {e}"),
                }
            }
            match router.recognize(&q).await {
                Ok(hits) => hits,
                Err(e) => {
                    tracing::warn!("time-aware memory recall failed: {e}");
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        let recall_latency_us = recall_start.elapsed().as_micros() as i64;

        // Mark seeded after the first full recall so subsequent turns
        // use a smaller limit.
        if !is_seeded {
            self.memory_recall_seeded.store(true, std::sync::atomic::Ordering::Release);
        }

        // Phase 18 (P1-1): record recall_audit row — gives operators a
        // window into recall quality without inspecting logs. Skipped
        // silently if the embedded backend isn't wired (e.g. server
        // mode with remote backends only).
        if let Some(ref router) = self.memory_router {
            if let Some(embedded) = router.find_embedded_backend() {
                let query_hash = {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};
                    let mut h = DefaultHasher::new();
                    query.hash(&mut h);
                    format!("{:016x}", h.finish())
                };
                if let Err(e) = embedded.record_recall_audit(
                    session_id,
                    query,
                    &query_hash,
                    structured_hits.len() as i64,
                    recall_latency_us,
                ) {
                    tracing::debug!(
                        target: "hermix.agent",
                        error = %e,
                        "recall_audit insert failed (non-fatal)"
                    );
                }
            }
        }

        // Phase 18 (P2-2): dedup hits by content (recurring facts
        // surface from frames + facts + memory_tool). Keep the
        // highest-salience copy and merge access_count if visible.
        let mut deduped: Vec<MemoryHit> = Vec::with_capacity(structured_hits.len());
        for h in structured_hits {
            if let Some(existing) = deduped.iter_mut().find(|e| e.content == h.content) {
                // Boost: take max salience; bump access_count via the
                // embedded backend so frequently-shown hits naturally
                // float up.
                existing.salience = existing.salience.max(h.salience);
                if let Some(ref router) = self.memory_router {
                    if let Some(embedded) = router.find_embedded_backend() {
                        if let Some(fact_key) = h.metadata.get("canonical_key")
                            .and_then(|v| v.as_str())
                        {
                            let _ = embedded.boost_fact_access(fact_key);
                        }
                    }
                }
            } else {
                deduped.push(h);
            }
        }
        // Sort by salience desc so the most relevant hits land in the
        // limited-char budget first.
        deduped.sort_by(|a, b| {
            b.salience
                .partial_cmp(&a.salience)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Inject structured hits as a markdown block with token budget.
        // M2: char budget + salience floor come from config (defaults
        // 4000 chars / 0.30 salience — see AgentConfig).
        let mut block = String::from("## Memory\n\n");
        let mut total_chars = 0;

        // Include MEMORY.md content as high-priority hits (file-based memory)
        if let Some(ref hh) = self.config.hermix_home {
            let memory_path = std::path::Path::new(hh).join("memories").join("MEMORY.md");
            if let Ok(content) = std::fs::read_to_string(&memory_path) {
                let trimmed = content.trim();
                if !trimmed.is_empty() {
                    let line = format!("- [source=memo] {}\n", trimmed);
                    if total_chars + line.len() <= char_budget {
                        block.push_str(&line);
                        total_chars += line.len();
                    }
                }
            }
        }

        for h in &deduped {
            // M2: salience noise floor — drop low-signal fragments
            // before they consume char_budget.
            if h.salience < min_salience {
                continue;
            }
            // Source-aware label: facts get `[fact]`, memory_write
            // gets `[memo]`, turns get `[turn]`, otherwise generic.
            let source_tag = match h.source.as_str() {
                "curation" | "fact_extractor" => "[fact]",
                "memory_tool" => "[memo]",
                "turn" => "[turn]",
                "session" => "[session]",
                other => {
                    // unknown source — generic tag, but include the
                    // raw value so debugging is easy.
                    if other.is_empty() { "" } else { "[src]" }
                }
            };
            let line = format!("- {} [salience={:.2}] {}\n", source_tag, h.salience, h.content);
            if total_chars + line.len() > char_budget {
                break;
            }
            block.push_str(&line);
            total_chars += line.len();
        }
        block
    }

    fn memory_sync(&self, user: &str, assistant: &str, session_id: &str) {
        if self.config.skip_memory {
            return;
        }
        // Route to MemoryRouter (all backends) — replaces legacy
        // `PluginManager::memory_providers` fan-out.
        if let Some(ref router) = self.memory_router {
            router.sync_turn(user, assistant, session_id);
        }
        // Phase 18 (P0-3): facts 表 caller — 从 user turn 抽取关键陈述
        // 并写入 SQLite `facts` 表，让跨 session 召回（line 1692 的
        // `MemoryRouter::recognize`）能命中 user-asserted facts。
        // 仅对 embedded backend 调用（其他 backend 没 facts 表）。
        if let Some(ref router) = self.memory_router {
            if let Some(embedded) = router.find_embedded_backend() {
                let facts = hermix_focus::fact_extractor::extract_facts_from_turn(
                    user, session_id, 0,
                );
                for fact in facts {
                    if let Err(e) = embedded.upsert_fact(
                        &fact.canonical_key,
                        fact.payload.clone(),
                        0.6, // user_asserted facts: slightly below memory_write (0.7)
                        &fact.tags,
                        "fact_extractor",
                    ) {
                        tracing::debug!(
                            target: "hermix.agent",
                            key = %fact.canonical_key,
                            error = %e,
                            "fact_extractor: upsert_fact failed"
                        );
                    }
                }
            }
        }

        // P2-6 (2026-06-27): dialectic user-style detection. We write
        // ONE fact per turn with `kind = user.style:<style>` so the
        // tag dedups across sessions via `canonical_key UNIQUE`. The
        // LLM never has to ask the user "are you direct or
        // exploratory?" — we infer it from the prompt shape and
        // surface it as a fact row that future turns can pull in.
        //
        // Best-effort: a `detect_user_style` miss (no rule hits
        // threshold) just means we don't write anything, which is
        // safer than a noisy tag competing for salience.
        if let Some(ref router) = self.memory_router {
            if let Some(embedded) = router.find_embedded_backend() {
                if let Some(detected) =
                    hermix_focus::user_style::detect_user_style(user)
                {
                    let key = format!("user.style:{}", detected.style);
                    let payload = serde_json::json!({
                        "kind": "user.style",
                        "style": detected.style,
                        "confidence": detected.confidence,
                        "evidence": detected.evidence,
                        "session_id": session_id,
                        "turn_number": 0,
                    });
                    let tags = vec![
                        format!("session:{session_id}"),
                        "user.style".to_string(),
                        "dialectic".to_string(),
                    ];
                    if let Err(e) = embedded.upsert_fact(
                        &key,
                        payload,
                        // Style facts sit at 0.5 — below preferences
                        // (0.6) but above ephemeral session metadata.
                        0.5,
                        &tags,
                        "user_style_detector",
                    ) {
                        tracing::debug!(
                            target: "hermix.agent",
                            key = %key,
                            error = %e,
                            "user_style: upsert_fact failed"
                        );
                    }
                }
            }
        }
    }

    /// Phase 18 (架构-1): freeze the current session state into a
    /// `session_snapshots` row. Called every N turns (default 50)
    /// and on `on_session_end`. The `frame_ids` and `summary` are
    /// extracted from the router's embedded backend so this method
    /// is a no-op when the embedded backend is absent.
    ///
    /// Designed to be cheap: the embedded SQLite path is O(visible
    /// frames) on `total_turns.is_multiple_of(N)` boundary calls,
    /// and O(1) on all other turns.
    fn maybe_freeze_snapshot(&self, session_id: &str, turn_number: u32) {
        if self.config.skip_memory {
            return;
        }
        let Some(ref router) = self.memory_router else {
            return;
        };
        let Some(embedded) = router.find_embedded_backend() else {
            return;
        };
        // Snapshot at every 50 turns + at the final turn. Avoid
        // snapshotting every turn (would saturate the table).
        const SNAPSHOT_INTERVAL: u32 = 50;
        let should_snapshot = turn_number > 0
            && (turn_number.is_multiple_of(SNAPSHOT_INTERVAL)
                || turn_number == self.config.max_turns);
        if !should_snapshot {
            return;
        }
        // Build a short summary of the most recent turn. We pull
        // the user message verbatim from the in-process message
        // context if available; otherwise we fall back to a
        // terse placeholder.
        let summary = format!(
            "Frozen at turn {turn_number} of session {session_id}"
        );
        let frame_ids: Vec<String> = Vec::new();
        match embedded.record_session_snapshot(
            session_id,
            turn_number,
            &summary,
            &frame_ids,
        ) {
            Ok(_) => tracing::debug!(
                target: "hermix.agent",
                session_id,
                turn_number,
                "snapshot frozen"
            ),
            Err(e) => tracing::warn!(
                target: "hermix.agent",
                session_id,
                turn_number,
                error = %e,
                "snapshot freeze failed"
            ),
        }
    }

    /// Phase 18 (架构-2): enforce a hard cap on visible frames so
    /// memory.db doesn't grow unbounded. Called every 100 turns as a
    /// background cleanup. Default cap is 10000 — enough for ~3
    /// months of heavy chat, soft-hidden rows stay on disk for the
    /// audit trail and the 30-day janitor.
    fn maybe_enforce_frame_cap(&self, turn_number: u32) {
        if self.config.skip_memory {
            return;
        }
        const CAP_INTERVAL: u32 = 100;
        const FRAME_CAP: usize = 10_000;
        if !turn_number.is_multiple_of(CAP_INTERVAL) || turn_number == 0 {
            return;
        }
        let Some(ref router) = self.memory_router else {
            return;
        };
        let Some(embedded) = router.find_embedded_backend() else {
            return;
        };
        match embedded.enforce_frame_cap(FRAME_CAP) {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                target: "hermix.agent",
                hidden = n,
                cap = FRAME_CAP,
                "frame cap enforced (oldest/lowest-salience hidden)"
            ),
            Err(e) => tracing::warn!(
                target: "hermix.agent",
                error = %e,
                "frame cap enforcement failed"
            ),
        }
    }

    fn memory_write_event_from_tool_call(tc: &ToolCall) -> Option<(String, String, String)> {
        if tc.function.name != "memory" {
            return None;
        }
        let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or("")
            .to_lowercase();
        if action != "add" && action != "replace" && action != "remove" {
            return None;
        }
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("memory")
            .to_string();
        let content = if action == "remove" {
            args.get("old_text")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        } else {
            args.get("content")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .unwrap_or("")
                .to_string()
        };
        Some((action, target, content))
    }

    fn notify_memory_writes(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        if self.config.skip_memory {
            return;
        }
        // Route `memory` tool calls (action=add/replace/remove) through
        // MemoryRouter — replaces legacy `PluginManager::memory_providers`
        // fan-out.
        let router = match self.memory_router {
            Some(ref r) => r,
            None => return,
        };
        for result in results {
            if result.is_error {
                continue;
            }
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
                continue;
            };
            let Some((action, target, content)) = Self::memory_write_event_from_tool_call(tc)
            else {
                continue;
            };
            router.on_memory_write(&action, &target, &content);
        }
    }

    fn delegation_event_from_tool_result(
        tc: &ToolCall,
        result: &ToolResult,
    ) -> Option<(String, String)> {
        if tc.function.name != "delegate_task" || result.is_error {
            return None;
        }
        let args: Value = serde_json::from_str(&tc.function.arguments).ok()?;
        let task = args
            .get("task")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?
            .to_string();

        let sub_agent_id = serde_json::from_str::<Value>(&result.content)
            .ok()
            .and_then(|v| {
                v.get("sub_agent_id")
                    .and_then(|id| id.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(ToString::to_string)
            })
            .unwrap_or_default();

        Some((task, sub_agent_id))
    }

    fn notify_delegations(&self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        if self.config.skip_memory {
            return;
        }
        // Route on_delegation hooks through MemoryRouter.
        let router = match self.memory_router {
            Some(ref r) => r,
            None => return,
        };
        for result in results {
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
                continue;
            };
            let Some((task, sub_agent_id)) = Self::delegation_event_from_tool_result(tc, result)
            else {
                continue;
            };
            router.on_delegation(&task, &sub_agent_id);
        }
        // M1: pop the matching `Delegation` frame and ship its
        // conclusion to the memory router. The push side is the
        // tool-dispatch path's `note_delegation_start` call (not
        // wired in M1.3 — public method is ready for the next
        // pass). When the push side is wired, the pop here will
        // always find a matching frame; until then it is a safe
        // no-op.
        for result in results {
            let Some(tc) = tool_calls.iter().find(|tc| tc.id == result.tool_call_id) else {
                continue;
            };
            if tc.function.name != "delegate_task" {
                continue;
            }
            let args: Option<Value> = serde_json::from_str(&tc.function.arguments).ok();
            let sub_agent_id = args
                .as_ref()
                .and_then(|v| v.get("sub_agent_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_default();
            // The result.content for a delegate_task that the
            // orchestrator handled contains the sub_agent_id; for
            // the legacy signal-envelope path, the args might
            // carry it. We re-parse the result.content as a
            // best-effort second source.
            let result_sub = serde_json::from_str::<Value>(&result.content)
                .ok()
                .and_then(|v| v.get("sub_agent_id").and_then(|id| id.as_str()).map(str::to_string));
            let effective_sub = result_sub
                .filter(|s| !s.is_empty())
                .or(if sub_agent_id.is_empty() { None } else { Some(sub_agent_id) })
                .unwrap_or_default();
            let status = if result.is_error {
                SessionFrameStatus::Failed
            } else {
                SessionFrameStatus::Completed
            };
            let _ = self.note_delegation_end(&effective_sub, status);
        }
        self.drain_focus_conclusions_to_memory_router();

        // M1.6.3: broadcast the current focus depth to the
        // consciousness tracker (if wired). This is a no-op when no
        // tracker is attached (e.g. tests, headless runs).
        if let Some(ref tracker) = self.consciousness_tracker {
            tracker.set_focus_depth(self.current_focus_depth());
        }
    }

    fn memory_on_turn_start(&self, turn: u32, message: &str) {
        if self.config.skip_memory {
            return;
        }
        if let Some(ref router) = self.memory_router {
            router.on_turn_start(turn, message);
        }
    }

    fn memory_system_prompt(&self) -> String {
        if self.config.skip_memory {
            return String::new();
        }
        if let Some(ref router) = self.memory_router {
            router.collect_system_prompt_blocks()
        } else {
            String::new()
        }
    }

    fn memory_pre_compress_note(&self, messages: &[Message]) -> Option<String> {
        if self.config.skip_memory {
            return None;
        }
        let router = match self.memory_router {
            Some(ref r) => r,
            None => return None,
        };
        let as_values: Vec<Value> = messages
            .iter()
            .filter_map(|m| serde_json::to_value(m).ok())
            .collect();
        let combined = router.on_pre_compress(&as_values);
        if combined.trim().is_empty() { None } else { Some(combined) }
    }

    /// Build a short human-readable summary of the session from the
    /// last user turn + last assistant turn. Used by
    /// [`Self::memory_on_session_end`] to write a session-level
    /// `FocusConclusion` to the embedded SQLite (memory.db) so
    /// cross-session recall works even when the session never used
    /// `delegate_task`.
    ///
    /// Returns `None` when the messages vector has no user content
    /// (e.g. an aborted / empty session) — in that case writing a
    /// meaningless empty row would only pollute recall.
    fn build_session_summary(messages: &[Message]) -> Option<String> {
        let last_user = messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::User)?;
        let user_text = last_user.content.as_deref().unwrap_or("").trim();
        if user_text.is_empty() {
            return None;
        }
        let user_truncated = if user_text.chars().count() > 200 {
            format!("{}…", hermix_core::text_utils::truncate_chars(user_text, 200))
        } else {
            user_text.to_string()
        };
        let last_assistant = messages
            .iter()
            .rev()
            .find(|m| m.role == MessageRole::Assistant)
            .and_then(|m| m.content.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        match last_assistant {
            Some(a) => {
                let a_truncated = if a.chars().count() > 400 {
                    format!("{}…", hermix_core::text_utils::truncate_chars(a, 400))
                } else {
                    a.to_string()
                };
                Some(format!("USER: {user_truncated}\nASSISTANT: {a_truncated}"))
            }
            None => Some(format!("USER: {user_truncated}")),
        }
    }

    fn memory_on_session_end(&self, messages: &[Message]) {
        if self.config.skip_memory {
            return;
        }
        // M1.6.5: persist a session-level FocusConclusion to the
        // embedded SQLite (memory.db) so cross-session recall works
        // even when the session never used `delegate_task`. Without
        // this hook the only writes to memory.db came from
        // `note_delegation_end`, leaving single-shot sessions with
        // 0 rows — the symptom the user reported as "会话记忆不工作".
        if let Some(ref router) = self.memory_router {
            if let Some(text) = Self::build_session_summary(messages) {
                let router = router.clone();
                let session_id = self.config.session_id.clone().unwrap_or_default();
                let conclusion = hermix_focus::FocusConclusion {
                    source_frame_id: uuid::Uuid::new_v4(),
                    text,
                    salience: 0.5,
                    tags: vec!["session".to_string()],
                    created_at: chrono::Utc::now(),
                };
                tokio::spawn(async move {
                    if let Err(e) = router.upsert_conclusion(&conclusion).await {
                        tracing::warn!(
                            target: "hermix.agent",
                            session_id = %session_id,
                            error = %e,
                            "memory_on_session_end: upsert_conclusion failed"
                        );
                    }
                });
            }
        }

        // Drain any pending focus conclusions.
        let drained = self.drain_focus_conclusions_to_memory_router();
        if drained > 0 {
            tracing::debug!(
                target: "hermix.agent",
                count = drained,
                "memory_on_session_end: drained leftover focus conclusions"
            );
        }

        // Route session-end notification through MemoryRouter.
        if let Some(ref router) = self.memory_router {
            let as_values: Vec<Value> = messages
                .iter()
                .filter_map(|m| serde_json::to_value(m).ok())
                .collect();
            router.on_session_end(&as_values);
        }
    }

    /// M1: collect the typed, route-gated directions that will be
    /// evaluated against the active route context and rendered into the
    /// system prompt. Each direction is one inline `if/else` branch
    /// extracted from the previous monolith of `build_system_prompt` —
    /// see `context::DirRoute` for the gate logic.
    fn collect_directions(
        &self,
        route_ctx: &crate::context::RouteContext<'_>,
    ) -> Vec<crate::context::SystemDir> {
        use crate::context::{DirRoute, SystemDir};
        let user_message = route_ctx.user_message;
        let tool_names = route_ctx.tool_names;
        let model_for_prompt = route_ctx.model_name;
        let mut dirs: Vec<SystemDir> = Vec::new();
        // Formatting discipline.
        if user_message.is_empty() {
            dirs.push(SystemDir::new(
                "format.full",
                DirRoute::FirstTurn,
                FORMATTING_DISCIPLINE_GUIDANCE,
            ));
        } else {
            dirs.push(SystemDir::new(
                "format.short",
                DirRoute::Subsequent,
                "## Format\nUse markdown. Keep replies concise.",
            ));
        }
        // Thinking discipline.
        if user_message.is_empty() {
            dirs.push(SystemDir::new(
                "think.full",
                DirRoute::FirstTurn,
                THINKING_DISCIPLINE_GUIDANCE,
            ));
        } else {
            dirs.push(SystemDir::new(
                "think.short",
                DirRoute::Subsequent,
                "## Thinking\nUse the configured reasoning block before your reply. Max 6 lines. Close before visible text.",
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
                "## Plan discipline\nBefore calling tools, briefly state which tool(s) you will use and what you expect to get back. Max 3 lines. One plan per user request.",
            ));
        }
        // Autonomous execution discipline (gated on opt-in).
        dirs.push(SystemDir::new(
            "autonomous.block",
            DirRoute::Autonomous,
            AUTONOMOUS_EXECUTION_GUIDANCE,
        ));
        // Browser tool-selection discipline.
        dirs.push(SystemDir::new(
            "browser.discipline",
            DirRoute::HasBrowser,
            BROWSER_TOOL_SELECTION_GUIDANCE,
        ));

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
        if tool_names.contains("compact_tool_chain") {
            dirs.push(SystemDir::new(
                "context_budget.guide",
                DirRoute::HasBatchKeyword,
                CONTEXT_BUDGET_GUIDANCE,
            ));
        }
        // P2-6: USER_STYLE_GUIDANCE is always-on.
        dirs.push(SystemDir::new(
            "user_style.always",
            DirRoute::Always,
            USER_STYLE_GUIDANCE,
        ));
        // Tool failure discipline: prevent blind retry spam.
        dirs.push(SystemDir::new(
            "tool_failure.always",
            DirRoute::Always,
            "Tool failure rule: if a tool fails twice, switch approach or ask the user. Do NOT retry the same call.",
        ));

        // Tool enforcement + model-family extras.
        if !tool_names.is_empty() && self.should_inject_tool_enforcement(model_for_prompt) {
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
        dirs
    }

    fn should_inject_tool_enforcement(&self, model: &str) -> bool {
        crate::prompt_assembly::should_inject_tool_enforcement(model)
    }

    fn platform_hint_text(&self) -> Option<&'static str> {
        crate::prompt_assembly::platform_hint_text(&self.config)
    }

    fn effective_provider_for_prompt(&self, model: &str) -> Option<String> {
        crate::prompt_assembly::effective_provider_for_prompt(&self.config, model)
    }

    fn skills_system_prompt(&self, tool_names: &HashSet<&str>) -> Option<String> {
        crate::prompt_assembly::skills_system_prompt(&self.config, tool_names)
    }

    fn context_files_prompt(&self) -> Option<String> {
        crate::prompt_assembly::context_files_prompt(&self.config)
    }

    fn extract_provider_and_model<'a>(&self, model: &'a str) -> (String, &'a str) {
        crate::prompt_assembly::extract_provider_and_model(&self.config, model)
    }

    fn auth_tokens_path(&self) -> PathBuf {
        crate::runtime_provider::auth_tokens_path(self)
    }

    fn build_runtime_provider(
        &self,
        provider: &str,
        model_name: &str,
        route_base_url: Option<&str>,
        api_key_env_override: Option<&str>,
        explicit_api_key: Option<&str>,
        api_mode: Option<&ApiMode>,
        credential_pool: Option<&Arc<CredentialPool>>,
    ) -> Result<Arc<dyn LlmProvider>, AgentError> {
        crate::runtime_provider::build_runtime_provider(
            self,
            provider,
            model_name,
            route_base_url,
            api_key_env_override,
            explicit_api_key,
            api_mode,
            credential_pool,
        )
    }

    fn credential_pool_for_route<'a>(
        &'a self,
        rt: &'a TurnRuntimeRoute,
    ) -> Option<&'a Arc<CredentialPool>> {
        crate::runtime_provider::credential_pool_for_route(self, rt)
    }

    fn messages_for_api_call(&self, ctx: &ContextManager) -> Vec<Message> {
        crate::prompt_assembly::messages_for_api_call(&self.config, ctx)
    }

    /// Build the full system prompt including identity, memory, and plugin context.
    ///
    /// Aligns with Python behavior:
    /// - prefer `~/.hermix/SOUL.md` as identity
    /// - fallback to `DEFAULT_AGENT_IDENTITY`
    /// - then append optional configured `system_prompt`
    #[instrument(
        name = "build_system_prompt",
        level = "debug",
        skip_all,
        fields(
            model = %model_for_prompt,
            tool_count = tool_schemas.len(),
            out_chars = tracing::field::Empty,
        )
    )]
    fn build_system_prompt(
        &self,
        user_message: &str,
        tool_schemas: &[ToolSchema],
        model_for_prompt: &str,
    ) -> String {
        let soul = load_soul_md();
        let mut builder = SystemPromptBuilder::new().with_personality(soul.as_deref());
        if let Some(base) = self.config.system_prompt.as_deref() {
            builder = builder.with_system_message(base);
        }
        // ★ directions[] — M1: collapse the six inline if/else cascades
        //   below (format/think/tools/plan/autonomous/browser + tool-family
        //   extras) into a typed route-gated queue assembled in
        //   `context::DirRoute::matches`. The audit log is emitted via
        //   `hermix.directions` tracing target and surfaced to the
        //   frontend through `system_prompt_directions` hook events.
        use crate::context::RouteContext;
        let tool_names_set: std::collections::HashSet<&str> =
            tool_schemas.iter().map(|t| t.name.as_str()).collect();
        let route_ctx = RouteContext {
            user_message,
            tool_names: &tool_names_set,
            model_name: model_for_prompt,
            autonomous_mode: self.config.autonomous_mode,
        };
        let directions = self.collect_directions(&route_ctx);
        builder = builder.with_directions(directions);
        builder.assemble_directions(&route_ctx);
        let audit = builder.direction_audit();
        let fired_count = audit.iter().filter(|a| a.fired).count();
        let skipped_count = audit.len() - fired_count;
        tracing::Span::current().record("dir_fired", fired_count);
        tracing::Span::current().record("dir_skipped", skipped_count);
        if !audit.is_empty() {
            tracing::debug!(
                target: "hermix.directions",
                fired = fired_count,
                skipped = skipped_count,
                "build_system_prompt direction audit ({} fired / {} skipped)",
                fired_count,
                skipped_count
            );
        }
        // The legacy `with_tool_guidance` channel is no longer populated
        // here — guidance blocks (memory / session_search / user_style /
        // tool_failure) now flow through `directions[]` and live in `parts`.
        // The audit log records which fired; the routing surface itself
        // remains the single source of truth.

        // ★ Inject the active tool list as a `<tools>` text block so the model
        //   is *always* aware of what it can call — even if the native
        //   OpenAI/Anthropic `tools` API field is ignored, filtered out by
        //   the router, or the model is a text-only fine-tune that has no
        //   notion of tool-calling wire protocols. Without this block, the
        //   model answers "I don't have terminal/execute_code/list_files
        //   tools" and refuses work it is fully equipped to do.
        builder = builder.with_tools_section(tool_schemas);

        if let Some(ref personality) = self.config.personality {
            let requested = personality.trim();
            if !requested.is_empty() {
                if let Some(profile) =
                    resolve_personality(requested, self.config.hermix_home.as_deref())
                {
                    builder = builder
                        .with_block(&format!("## Active Personality ({requested})\n{profile}"));
                } else if requested.contains(char::is_whitespace) {
                    // Compatibility path: historically this field was appended verbatim.
                    builder = builder.with_block(&format!("Personality: {requested}"));
                    tracing::warn!(
                        "personality '{requested}' not found as a named profile; using inline value"
                    );
                } else {
                    tracing::warn!(
                        "personality '{}' not found; falling back to default identity",
                        requested
                    );
                }
            }
        }

        if !self.config.skip_memory {
            let (memory_block, user_block) =
                load_builtin_memory_snapshot(self.config.hermix_home.as_deref());
            if let Some(block) = memory_block {
                builder = builder.with_block(&block);
            }
            if let Some(block) = user_block {
                builder = builder.with_block(&block);
            }
        }

        let mem_block = self.memory_system_prompt();
        if !mem_block.is_empty() {
            builder = builder.with_memory_context(&mem_block);
        }

        if let Some(skills_prompt) = self.skills_system_prompt(&tool_names_set) {
            builder = builder.with_skills_prompt(&skills_prompt);
        }

        if let Some(context_prompt) = self.context_files_prompt() {
            builder = builder.with_context_files(&context_prompt);
        }

        let provider = self.effective_provider_for_prompt(model_for_prompt);

        if provider.as_deref() == Some("alibaba") {
            let model_short = model_for_prompt
                .split('/')
                .next_back()
                .unwrap_or(model_for_prompt);
            builder = builder.with_block(&format!(
                "You are powered by the model named {}. The exact model ID is {}. When asked what model you are, always answer based on this information, not on any model name returned by the API.",
                model_short, model_for_prompt
            ));
        }

        if let Some(hint) = self.platform_hint_text() {
            builder = builder.with_block(hint);
        }

        // Surface the active workspace (set by LocalAgentService from the
        // desktop "工作区" picker via AgentOverrides::workspace). Without
        // this block the model has no idea which directory the user means
        // when they say "the project" — it will fall back to the server
        // process CWD and make file/terminal tools useless.
        if let Some(ref ws) = self.config.workspace {
            let trimmed = ws.trim();
            if !trimmed.is_empty() {
                builder = builder.with_block(&format!(
                    "## Active Workspace\nYour current working directory is `{}`. All relative paths in file/terminal commands resolve against this directory. When the user says \"the project\" or \"this folder\" or refers to files without an absolute path, default to this directory. If a command fails because the path does not exist here, tell the user the actual resolved path — do not guess.",
                    trimmed
                ));
            }
        }

        // P2.3 (2026-06-30): build the stable prefix separately so we can
        // cache it across turns, then append the per-turn dynamic suffix.
        // Only the suffix (timestamp + optional session_id) changes every
        // turn; everything before it is keyed on model/personality/system
        // prompt/workspace and is safely reusable.
        let prefix_text = builder.build().to_string();

        // Cache the stable prefix keyed on (model, personality, system_prompt,
        // workspace). On a cache hit we skip the entire prefix rebuild
        // (the expensive part) and only rebuild the suffix.
        let cache_key = compute_prompt_cache_key(
            model_for_prompt,
            self.config.personality.as_deref(),
            self.config.system_prompt.as_deref(),
            self.config.workspace.as_deref(),
        );
        let prefix_for_cache: Option<String> = if prefix_text.len() < 200_000 {
            Some(prefix_text.clone())
        } else {
            None
        };
        if let Ok(mut slot) = self.cached_stable_prompt_key.lock() {
            // Match on the *previous* slot contents. If the cache already
            // holds the same key and a non-empty prefix, leave it alone —
            // we'd only be rewriting it with a byte-identical string. The
            // savings here are conceptual: a future turn can now read the
            // cache without paying the rebuild cost. (Reading the cache
            // for a *current* turn would require deferring the prefix build
            // entirely; that's a separate optimization that the agent loop
            // already does at a higher level via `stored_system_prompt`.)
            match &*slot {
                Some((cached_key, cached_prefix))
                    if *cached_key == cache_key && !cached_prefix.is_empty() =>
                {
                    // Cache hit on a populated slot — keep it.
                }
                _ => {
                    *slot = prefix_for_cache.map(|p| (cache_key, p));
                }
            }
        }

        // Per-turn dynamic suffix — rebuilt every call, never cached.
        let suffix_text = self.build_dynamic_suffix(model_for_prompt);

        let prompt = format!("{prefix_text}{suffix_text}");
        tracing::Span::current().record("out_chars", prompt.len());

        prompt
    }

    /// Build the per-turn dynamic suffix of the system prompt.
    ///
    /// This is the only part of the system prompt that changes every turn:
    ///   - the timestamp block (model + provider name)
    ///   - the optional session_id line (when `pass_session_id` is on)
    ///
    /// The rest of the prompt (identity, directions, tools, personality,
    /// memory, skills, context files, model branding, platform hint,
    /// workspace) lives in the stable prefix and is reused from
    /// `cached_stable_prompt_key` across turns when its key matches.
    fn build_dynamic_suffix(&self, model_for_prompt: &str) -> String {
        crate::prompt_assembly::build_dynamic_suffix(&self.config, model_for_prompt)
    }

    /// Returns `(prompt, restored_from_storage)` — restored prompts skip fresh `build_system_prompt`.
    fn resolve_initial_system_prompt(
        &self,
        user_message: &str,
        tool_schemas: &[ToolSchema],
    ) -> (String, bool) {
        let (prompt, restored) = if let Some(ref s) = self.config.stored_system_prompt {
            let t = s.trim();
            if !t.is_empty() {
                (s.clone(), true)
            } else {
                (
                    self.build_system_prompt(user_message, tool_schemas, &self.config.model),
                    false,
                )
            }
        } else {
            (
                self.build_system_prompt(user_message, tool_schemas, &self.config.model),
                false,
            )
        };
        // ★ M2: protocol-completeness single choke point. Audit the
        //   produced prompt against the section gate; if any required
        //   marker is missing, splice in the default body. This is the
        //   *only* function in `agent_loop` that mutates the system
        //   prompt for completeness — every other code path that builds
        //   a prompt must funnel through here.
        let (final_prompt, _report) = {
            use crate::context::{RouteContext, Section};
            let tool_names_set: std::collections::HashSet<&str> =
                tool_schemas.iter().map(|t| t.name.as_str()).collect();
            let route_ctx = RouteContext {
                user_message,
                tool_names: &tool_names_set,
                model_name: &self.config.model,
                autonomous_mode: self.config.autonomous_mode,
            };
            let (patched, report) = Section::audit_and_patch(&prompt, &route_ctx);
            if report.had_patches() {
                tracing::warn!(
                    target: "hermix.section_gate",
                    patched = report.patched.len(),
                    pending = report.pending.len(),
                    restored_from_storage = restored,
                    "SectionGate auto-patched {} missing required sections",
                    report.patched.len()
                );
            } else if !report.pending.is_empty() {
                tracing::debug!(
                    target: "hermix.section_gate",
                    pending = report.pending.len(),
                    "SectionGate: {} optional sections absent (not required for this turn)",
                    report.pending.len()
                );
            }
            // 2026-06-29 (C3 marker protocol): forward the audit report
            // to the upstream `on_section_gate` callback so the trace
            // panel can render "SectionGate patched Identity, Timestamp"
            // in real time. We forward on *every* call (even when
            // nothing was patched) so the panel can show the "all
            // sections present" state. The callback is best-effort
            // — failure to serialise is logged and swallowed.
            if let Some(cb) = self.callbacks.on_section_gate.as_ref() {
                cb(&report);
            }
            (patched, report)
        };
        (final_prompt, restored)
    }

    /// Compress when total chars exceed 80% of the context budget (Python auto-compaction).
    ///
    /// Uses the advanced LLM-powered compressor when available (set via
    /// `with_compressor`), falling back to the simple character-based
    /// compressor otherwise.
    ///
    /// v2 (2026-06-21): before the LLM compressor replaces the middle
    /// of the conversation with a summary, we **archive** the same
    /// slice to disk via [`archive_context_section`]. The summary
    /// becomes lossy-by-design; the archive file is the lossless
    /// evidence the user can re-open from the chat UI. The archive
    /// refs are stashed in `last_archive_refs` for the frontend.
    ///
    /// v3 (2026-06-29): also emit `compaction_status` snapshots at
    /// the pressure-detected and post-attempt boundaries so the
    /// `CompactionStatusBanner` can render in real time. We clone
    /// the snapshot before dropping the compressor lock so the
    /// callback can fire on the upstream IO task without holding
    /// the compressor's `MutexGuard`.
    async fn auto_compress_if_over_threshold(&self, ctx: &mut ContextManager) {
        let total_tokens = ctx.total_tokens();
        let max_c = ctx.max_context_tokens().max(1);
        let threshold = ctx.pressure_threshold_tokens();
        if total_tokens <= threshold {
            return;
        }
        let message = format!(
            "Context pressure at {}%, triggering compression",
            (total_tokens * 100) / max_c
        );
        tracing::info!("{message}");
        self.emit_status("lifecycle", &message);

        // 2026-06-29: pressure snapshot — fired before any compression
        // attempt so the banner can show "即将触发自动压缩" the moment
        // we cross the threshold, not only after the LLM call lands.
        if let Some(ref compressor) = self.compression {
            let mut guard: tokio::sync::MutexGuard<'_, crate::compression::ContextCompressor> =
                compressor.lock().await;
            let pre_snap = guard.breaker_snapshot(
                total_tokens as u64,
                threshold as u64,
                None,
            );
            drop(guard);
            self.emit_compaction_status(&pre_snap);
        }

        // v2: unload the middle of the conversation to disk *before*
        // the LLM compressor runs. Failures are logged, never fatal —
        // the compressor is still our safety net.
        let hermix_home = hermix_config::paths::hermix_home();
        // Forward the session id so the server endpoint
        // `GET /v1/sessions/{id}/archive-refs` can list what we wrote
        // without re-scanning every archive file. Falls back to `None`
        // (no per-session index) when this loop was constructed without
        // a session_id — e.g. the test scaffolding path.
        let session_id = self.config.session_id.as_deref();
        match archive_context_section(ctx.get_messages(), &hermix_home, None, session_id) {
            Ok(refs) if !refs.is_empty() => {
                tracing::info!(
                    "archived {} messages ({} chars) before compression",
                    refs.len(),
                    refs.iter().map(|r| r.char_count).sum::<usize>(),
                );
                let summary = render_archive_summary(&refs);
                // Surface the refs to the LLM as a guidance block so
                // it knows where to "look up" older context if asked.
                ctx.add_message(Message::system(summary));
                if let Ok(mut slot) = self.last_archive_refs.lock() {
                    *slot = refs;
                }
            }
            Ok(_) => {
                // Empty slice (system + tail only): nothing to unload.
            }
            Err(e) => {
                tracing::warn!("archive_context_section failed: {e}");
            }
        }

        if let Some(note) = self.memory_pre_compress_note(ctx.get_messages()) {
            ctx.add_message(Message::system(note));
        }

        // Prefer the advanced LLM-powered compressor when available.
        if let Some(ref compressor) = self.compression {
            let messages = ctx.get_messages().to_vec();
            let mut guard: tokio::sync::MutexGuard<'_, crate::compression::ContextCompressor> = compressor.lock().await;
            let compressed = guard.compress(messages, None).await;
            // Store the compressed summary for cross-session persistence.
            if let Some(prev) = guard.previous_summary() {
                if let Ok(mut latest) = self.latest_compressed_summary.lock() {
                    *latest = Some(prev.to_string());
                }
            }
            // 2026-06-26 (A + B): stash the raw archive + metrics
            // from this compression so `local_agent_service` can
            // hand them to `save_session_atomic_with_compression` in
            // the same DB transaction that updates
            // `sessions.compressed_summary`. Without this capture,
            // the in-memory conversation would be the only place
            // that knew what got compressed — and `flush_messages`
            // would have wiped those rows from `messages` by the
            // time the agent loop returns to the caller.
            let archived = guard.last_archived_turns().to_vec();
            if !archived.is_empty() {
                if let Ok(mut slot) = self.latest_compression_archived_turns.lock() {
                    *slot = Some(archived);
                }
            }
            if let Ok(mut slot) = self.latest_compression_metrics.lock() {
                *slot = Some(guard.last_metrics().clone());
            }
            // 2026-06-29: post-attempt snapshot. We classify success
            // by `summary_errors == 0` (compress() records an error
            // in the metrics if the LLM auxiliary call failed) and
            // feed that to `record_compression_outcome` so the
            // breaker state machine actually transitions. Then we
            // snapshot the post-state for the banner.
            //
            // The whole post-processing chain happens *before* the
            // guard is dropped so the snapshot we report reflects
            // the exact state the breaker saw. The callback is
            // synchronous — it doesn't await anything — so the
            // temporary lifetime extension is fine.
            let post_snap = {
                // Snapshot the fields we need *before* taking the
                // mutable borrow on `guard` for
                // `record_compression_outcome` — Rust's NLL won't
                // allow the immutable borrow of `last_metrics()` to
                // overlap with the mutable call otherwise.
                let summary_errors = guard.last_metrics().summary_errors;
                let tokens_saved = guard.last_metrics().tokens_saved;
                let success = summary_errors == 0;
                let post_state = guard.record_compression_outcome(success);
                let used_after = if success && tokens_saved > 0 {
                    total_tokens.saturating_sub(tokens_saved as usize)
                } else {
                    total_tokens
                };
                let failure_reason = if !success {
                    Some("LLM summary generation failed (see server log)".to_string())
                } else {
                    None
                };
                let mut snap = guard.breaker_snapshot(
                    used_after as u64,
                    threshold as u64,
                    failure_reason,
                );
                // Pin the post-state so the banner shows the *post-attempt*
                // value even if `breaker_state()` re-derives a HalfOpen
                // because cooldown elapsed during the call.
                snap.state = post_state;
                snap
            };
            self.emit_compaction_status(&post_snap);
            *ctx.get_messages_mut() = compressed;
        } else {
            // Fallback: simple character-based compression.
            ctx.compress();
        }
    }

    fn emit_status(&self, event_type: &str, message: &str) {
        crate::message_helpers::emit_status(&self.config, &self.callbacks, event_type, message)
    }

    /// 2026-06-29: forward a `BreakerSnapshot` to whoever wired
    /// `AgentCallbacks::on_compaction_status`. The snapshot already
    /// carries every field the front-end `CompactionStatusBanner`
    /// needs (state, pressure, failure reason, change timestamp)
    /// so the upstream layer just JSON-serialises it and emits the
    /// `compaction_status` WS event.
    ///
    /// `quiet_mode` does NOT short-circuit this path: the banner is
    /// a UI affordance, and suppressing it would hide legitimate
    /// "context is overflowing" / "compression has been disabled"
    /// signals from the user.
    fn emit_compaction_status(&self, snapshot: &crate::compression::BreakerSnapshot) {
        crate::message_helpers::emit_compaction_status(&self.callbacks, snapshot)
    }

    fn assistant_visible_text(m: &Message) -> bool {
        crate::message_helpers::assistant_visible_text(m)
    }

    fn assistant_visible_text_after_think_blocks(m: &Message) -> bool {
        crate::message_helpers::assistant_visible_text_after_think_blocks(m)
    }

    fn assistant_has_reasoning(m: &Message) -> bool {
        crate::message_helpers::assistant_has_reasoning(m)
    }

    fn normalize_tool_call_arguments(tc: &mut ToolCall) -> Result<(), String> {
        crate::message_helpers::normalize_tool_call_arguments(tc)
    }

    fn extra_body_for_api_mode(&self, api_mode: &ApiMode) -> Option<Value> {
        crate::message_helpers::extra_body_for_api_mode(&self.config, api_mode)
    }

    // -- Retry-aware LLM call ---------------------------------------------

    fn call_llm_with_retry<'a>(
        &'a self,
        ctx: &'a ContextManager,
        tool_schemas: &'a [ToolSchema],
        route: Option<&'a TurnRuntimeRoute>,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = Result<hermix_core::LlmResponse, AgentError>>
                + Send
                + 'a,
        >,
    > {
        Box::pin(self.call_llm_with_retry_inner(ctx, tool_schemas, route))
    }

    async fn call_llm_with_retry_inner(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
    ) -> Result<hermix_core::LlmResponse, AgentError> {
        let model = route
            .map(|r| r.model.as_str())
            .unwrap_or(self.config.model.as_str());
        let (_, request_model) = self.extract_provider_and_model(model);
        let (_, primary_request_model) =
            self.extract_provider_and_model(self.config.model.as_str());
        if let Some(rt) = route {
            if let Some(ref label) = rt.route_label {
                tracing::debug!(%label, model = %rt.model, ?rt.signature, "smart model route");
            }
            if rt.command.is_some() || !rt.args.is_empty() {
                tracing::debug!(command = ?rt.command, args = ?rt.args, "smart route process metadata");
            }
        }
        let api_messages = self.messages_for_api_call(ctx);
        let retry = &self.config.retry;
        let (effective_max_retries, effective_base_delay_ms) =
            (retry.max_retries, retry.base_delay_ms);
        let default_extra_body = self.extra_body_for_api_mode(&self.config.api_mode);

        for attempt in 0..=effective_max_retries {
            self.interrupt.check_interrupt()?;
            let pre_api_ctx = serde_json::json!({
                "attempt": attempt,
                "model": model,
                "stream": false,
                "route_label": route.and_then(|r| r.route_label.as_deref()),
            });
            let _ = self.invoke_hook(HookType::PreApiRequest, &pre_api_ctx);
            let result = if let Some(rt) = route {
                let (provider_name, model_name) = self.extract_provider_and_model(model);
                let mode = rt.api_mode.as_ref().unwrap_or(&self.config.api_mode);
                let extra_body_for_call = self.extra_body_for_api_mode(mode);
                let pool = self.credential_pool_for_route(rt);
                let routed_provider = self.build_runtime_provider(
                    rt.provider.as_deref().unwrap_or(provider_name.as_str()),
                    model_name,
                    rt.base_url.as_deref(),
                    rt.api_key_env.as_deref(),
                    None,
                    Some(mode),
                    pool,
                );
                match routed_provider {
                    Ok(provider) => {
                        provider
                            .chat_completion(
                                &api_messages,
                                tool_schemas,
                                self.config.max_tokens,
                                self.config.temperature,
                                Some(request_model),
                                extra_body_for_call.as_ref(),
                            )
                            .await
                    }
                    Err(e) => {
                        tracing::warn!(
                            "Runtime route unavailable (reason={:?}), falling back to primary runtime: {}",
                            rt.routing_reason,
                            e
                        );
                        self.llm_provider
                            .chat_completion(
                                &api_messages,
                                tool_schemas,
                                self.config.max_tokens,
                                self.config.temperature,
                                Some(primary_request_model),
                                default_extra_body.as_ref(),
                            )
                            .await
                    }
                }
            } else {
                self.llm_provider
                    .chat_completion(
                        &api_messages,
                        tool_schemas,
                        self.config.max_tokens,
                        self.config.temperature,
                        Some(request_model),
                        default_extra_body.as_ref(),
                    )
                    .await
            };
            let post_api_ctx = match &result {
                Ok(response) => serde_json::json!({
                    "attempt": attempt,
                    "model": model,
                    "stream": false,
                    "ok": true,
                    "finish_reason": response.finish_reason,
                    "has_tool_calls": response
                        .message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty()),
                }),
                Err(err) => serde_json::json!({
                    "attempt": attempt,
                    "model": model,
                    "stream": false,
                    "ok": false,
                    "error": err.to_string(),
                }),
            };
            let _ = self.invoke_hook(HookType::PostApiRequest, &post_api_ctx);

            match result {
                Ok(response) => return Ok(response),
                Err(e) => {
                    let err_str = e.to_string();
                    let class = classify_error(&err_str);
                    tracing::warn!(
                        attempt,
                        error_class = ?class,
                        "LLM API error: {}",
                        &err_str[..err_str.len().min(200)]
                    );

                    match class {
                        ErrorClass::Auth | ErrorClass::Fatal => {
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::ContextOverflow => {
                            return Err(AgentError::LlmApi(err_str));
                        }
                        ErrorClass::RateLimit | ErrorClass::Retryable => {
                            if attempt >= effective_max_retries {
                                if let Some(ref fallback) = retry.fallback_model {
                                    if request_model != fallback.as_str() {
                                        let (_, fallback_request_model) =
                                            self.extract_provider_and_model(fallback);
                                        tracing::info!(
                                            "All retries exhausted on {}. Trying fallback: {}",
                                            model,
                                            fallback
                                        );
                                        let fallback_result = self
                                            .llm_provider
                                            .chat_completion(
                                                &api_messages,
                                                tool_schemas,
                                                self.config.max_tokens,
                                                self.config.temperature,
                                                Some(fallback_request_model),
                                                default_extra_body.as_ref(),
                                            )
                                            .await;
                                        return fallback_result
                                            .map_err(|e| AgentError::LlmApi(e.to_string()));
                                    }
                                }
                                return Err(AgentError::LlmApi(err_str));
                            }
                            let delay = jittered_backoff(
                                attempt,
                                effective_base_delay_ms,
                                retry.max_delay_ms,
                            );
                            tracing::info!(
                                "Retrying in {}ms (attempt {}/{})",
                                delay.as_millis(),
                                attempt + 1,
                                effective_max_retries
                            );
                            self.emit_status(
                                "lifecycle",
                                &format!(
                                    "LLM API retry in {}ms (attempt {}/{})",
                                    delay.as_millis(),
                                    attempt + 1,
                                    effective_max_retries
                                ),
                            );
                            sleep(delay).await;
                        }
                    }
                }
            }
        }
        unreachable!()
    }

    fn assemble_stream_assistant_message(
        content: &str,
        reasoning_content: &str,
        tool_calls: &[ToolCall],
    ) -> Message {
        crate::stream_collect::assemble_stream_assistant_message(content, reasoning_content, tool_calls)
    }

    /// Apply one streaming [`StreamChunk`] during [`Self::collect_stream_llm_response`].
    ///
    /// `think_parser` is a stateful splitter that pulls `<think>...</think>` text
    /// out of the visible content stream in real time and routes it to
    /// `reasoning_content` instead. Without this, providers that embed their
    /// reasoning in `delta.content` (MiniMax M2.7, Qwen, DeepSeek-R1) would
    /// leak the whole thinking monologue into the user-visible reply.
    ///
    /// `tool_call_sanitizer` is a stateful redactor that pulls raw
    /// `<minimax:tool_call>…</minimax:tool_call>` /
    /// `<invoke name="…">…</invoke>` blocks out of the *clean* (post-think)
    /// text. Without it, providers that emit tool calls as raw XML inside
    /// `delta.content` (MiniMax, older Hermes fine-tunes) would leak the
    /// whole XML envelope to the user. See
    /// `hermix_core::tool_call_sanitizer` for the full state machine.
    fn apply_stream_collect_chunk(
        &self,
        chunk: StreamChunk,
        content: &mut String,
        raw_content: &mut String,
        reasoning_content: &mut String,
        think_parser: &mut ThinkBlockParser,
        tool_call_sanitizer: &mut ToolCallSanitizer,
        tool_calls: &mut Vec<ToolCall>,
        last_usage: &mut Option<UsageStats>,
        finish_reason: &mut Option<String>,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
    ) {
        crate::stream_collect::apply_stream_collect_chunk(
            &self.callbacks,
            chunk,
            content,
            raw_content,
            reasoning_content,
            think_parser,
            tool_call_sanitizer,
            tool_calls,
            last_usage,
            finish_reason,
            on_chunk,
        );
    }

    /// Collect one streaming completion into [`LlmResponse`] (first attempt in `run_stream` D-step).
    async fn collect_stream_llm_response(
        &self,
        ctx: &ContextManager,
        tool_schemas: &[ToolSchema],
        route: Option<&TurnRuntimeRoute>,
        active_model: &str,
        on_chunk: &(dyn Fn(StreamChunk) + Send + Sync),
    ) -> Result<StreamCollectOutcome, AgentError> {
        let api_messages = self.messages_for_api_call(ctx);
        let default_extra_body = self.extra_body_for_api_mode(&self.config.api_mode);
        let (_, active_request_model) = self.extract_provider_and_model(active_model);
        let (_, primary_request_model) =
            self.extract_provider_and_model(self.config.model.as_str());
        let mut stream = if let Some(rt) = route {
            let (provider_name, model_name) = self.extract_provider_and_model(active_model);
            let mode = rt.api_mode.as_ref().unwrap_or(&self.config.api_mode);
            let extra_body_for_call = self.extra_body_for_api_mode(mode);
            let pool = self.credential_pool_for_route(rt);
            match self.build_runtime_provider(
                rt.provider.as_deref().unwrap_or(provider_name.as_str()),
                model_name,
                rt.base_url.as_deref(),
                rt.api_key_env.as_deref(),
                None,
                Some(mode),
                pool,
            ) {
                Ok(provider) => provider.chat_completion_stream(
                    &api_messages,
                    tool_schemas,
                    self.config.max_tokens,
                    self.config.temperature,
                    Some(model_name),
                    extra_body_for_call.as_ref(),
                ),
                Err(e) => {
                    tracing::warn!(
                        "Runtime route unavailable (reason={:?}) for stream, falling back to primary runtime: {}",
                        rt.routing_reason,
                        e
                    );
                    self.llm_provider.chat_completion_stream(
                        &api_messages,
                        tool_schemas,
                        self.config.max_tokens,
                        self.config.temperature,
                        Some(primary_request_model),
                        default_extra_body.as_ref(),
                    )
                }
            }
        } else {
            self.llm_provider.chat_completion_stream(
                &api_messages,
                tool_schemas,
                self.config.max_tokens,
                self.config.temperature,
                Some(active_request_model),
                default_extra_body.as_ref(),
            )
        };

        let mut content = String::new();
        // Pre-sanitizer accumulator — holds the complete post-think,
        // pre-redaction text so we can parse it for hidden
        // `<minimax:tool_call>` blocks after the stream completes.
        let mut raw_content = String::new();
        let mut reasoning_content = String::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();
        let mut last_usage: Option<UsageStats> = None;
        let mut finish_reason: Option<String> = None;
        // Stateful splitter for providers that embed `<think>...</think>` in
        // `delta.content` instead of a separate reasoning field. One parser per
        // LLM call — its state is meaningless across turn boundaries.
        let mut think_parser = ThinkBlockParser::new();
        // Defense-in-depth streaming redactor. Some model families
        // (notably MiniMax / M2.7, MiniMax / Hermes fine-tunes) emit
        // tool calls as raw XML inside `delta.content` rather than
        // through the OpenAI `tools` field. The downstream redactor
        // in `hermix-server` also strips these blocks from the WS
        // payload, but doing it here means:
        //   1. The local `content` accumulator (which becomes the
        //      assistant's persisted message) is also clean, so
        //      reloading the session after a bad turn never shows
        //      leaked XML.
        //   2. The chunk that `apply_stream_collect_chunk` forwards
        //      to the WS callback already has the redacted text,
        //      so the WS layer's regex is a belt-and-suspenders
        //      pass, not the last line of defence.
        // See `hermix_core::tool_call_sanitizer` for the
        // state-machine details.
        let mut tool_call_sanitizer = ToolCallSanitizer::new();

        let activity_secs = self.config.stream_activity_interval_secs;

        if activity_secs == 0 {
            while let Some(chunk_result) = stream.next().await {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    let (flush_content, flush_reasoning) = think_parser.flush();
                    if !flush_content.is_empty() {
                        content.push_str(&flush_content);
                    }
                    if !flush_reasoning.is_empty() {
                        reasoning_content.push_str(&flush_reasoning);
                    }
                    // Drop any text the tool-call sanitizer was
                    // still holding inside an unterminated
                    // block. `ToolCallSanitizer::flush` does not
                    // emit held bytes — leaking them would defeat
                    // the whole redactor.
                    tool_call_sanitizer.flush();
                    let message = Self::assemble_stream_assistant_message(
                        &content,
                        &reasoning_content,
                        &tool_calls,
                    );
                    return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                        message,
                        usage: last_usage.clone(),
                        model: active_model.to_string(),
                        finish_reason: Some("interrupted".to_string()),
                    }));
                }
                let chunk = chunk_result?;
                self.apply_stream_collect_chunk(
                    chunk,
                    &mut content,
                    &mut raw_content,
                    &mut reasoning_content,
                    &mut think_parser,
                    &mut tool_call_sanitizer,
                    &mut tool_calls,
                    &mut last_usage,
                    &mut finish_reason,
                    on_chunk,
                );
            }
        } else {
            const IDLE_ARM_MAX: Duration = Duration::from_secs(86_400);
            let first_dur = Duration::from_secs(activity_secs);
            let stall_secs = self.config.stream_activity_stall_secs;
            let stall_dur = Duration::from_secs(stall_secs);

            'silence_watch: loop {
                let mut idle = Box::pin(sleep(first_dur));
                let mut warm_notice_sent = false;
                let mut stall_notice_sent = false;
                loop {
                    tokio::select! {
                        biased;
                        chunk_opt = stream.next() => {
                            match chunk_opt {
                                None => break 'silence_watch,
                                Some(chunk_result) => {
                                    if self.interrupt.take_interrupt_graceful().is_some() {
                                        let message = Self::assemble_stream_assistant_message(
                                            &content,
                                            &reasoning_content,
                                            &tool_calls,
                                        );
                                        return Ok(StreamCollectOutcome::Interrupted(LlmResponse {
                                            message,
                                            usage: last_usage.clone(),
                                            model: active_model.to_string(),
                                            finish_reason: Some("interrupted".to_string()),
                                        }));
                                    }
                                    let chunk = chunk_result?;
                                    self.apply_stream_collect_chunk(
                                        chunk,
                                        &mut content,
                                        &mut raw_content,
                                        &mut reasoning_content,
                                        &mut think_parser,
                                        &mut tool_call_sanitizer,
                                        &mut tool_calls,
                                        &mut last_usage,
                                        &mut finish_reason,
                                        on_chunk,
                                    );
                                    continue 'silence_watch;
                                }
                            }
                        }
                        _ = idle.as_mut() => {
                            if !warm_notice_sent {
                                self.emit_status(
                                    "activity",
                                    "Still waiting for the model…",
                                );
                                warm_notice_sent = true;
                                if stall_secs > 0 {
                                    idle.as_mut()
                                        .reset(tokio::time::Instant::now() + stall_dur);
                                } else {
                                    idle.as_mut()
                                        .reset(tokio::time::Instant::now() + IDLE_ARM_MAX);
                                }
                            } else if stall_secs > 0 && !stall_notice_sent {
                                self.emit_status(
                                    "activity",
                                    "Still no model output — the stream may be stuck.",
                                );
                                stall_notice_sent = true;
                                idle.as_mut()
                                    .reset(tokio::time::Instant::now() + IDLE_ARM_MAX);
                            } else {
                                idle.as_mut()
                                    .reset(tokio::time::Instant::now() + IDLE_ARM_MAX);
                            }
                        }
                    }
                }
            }
        }

        let has_truncated_tool_args = tool_calls.iter().any(|tc| {
            let trimmed = tc.function.arguments.trim();
            !trimmed.is_empty() && serde_json::from_str::<Value>(trimmed).is_err()
        });
        if has_truncated_tool_args {
            finish_reason = Some("length".to_string());
        }

        // Drain any look-ahead held by the think-block parser at end-of-stream.
        let (flush_content, flush_reasoning) = think_parser.flush();
        if !flush_content.is_empty() {
            content.push_str(&flush_content);
            raw_content.push_str(&flush_content);
        }
        if !flush_reasoning.is_empty() {
            reasoning_content.push_str(&flush_reasoning);
        }
        // Drop any text the tool-call sanitizer was still holding
        // inside an unterminated block. `ToolCallSanitizer::flush`
        // does not emit held bytes — leaking them would defeat the
        // whole redactor.
        tool_call_sanitizer.flush();

        // If the LLM returned zero structured tool calls but the raw
        // text contains `<minimax:tool_call>` XML blocks (MiniMax-M2,
        // MiniMax-M2.5, older Hermes fine-tunes), try to parse them
        // now. The sanitizer has already stripped the XML from
        // `content` (the visible text), so the user sees a clean
        // bubble; the parsed calls feed into the regular tool-
        // execution path.
        if tool_calls.is_empty() {
            let parsed = hermix_core::tool_call_parser::parse_tool_calls(&raw_content)
                .unwrap_or_default();
            if !parsed.is_empty() {
                tracing::info!(
                    parsed_count = parsed.len(),
                    raw_len = raw_content.len(),
                    "Parsed tool calls from raw XML text (MiniMax/M2.x compat)"
                );
                tool_calls = parsed;
            }
        }

        let message =
            Self::assemble_stream_assistant_message(&content, &reasoning_content, &tool_calls);

        Ok(StreamCollectOutcome::Complete(LlmResponse {
            message,
            usage: last_usage,
            model: active_model.to_string(),
            finish_reason,
        }))
    }

    /// Run the agent loop (non-streaming).
    ///
    /// Sends the initial messages to the LLM, then iteratively:
    /// - Executes any tool calls the LLM makes
    /// - Feeds results back as tool messages
    /// - Stops when the LLM responds without tool calls, or max turns exceeded
    ///
    /// Implementation is split into three helpers:
    /// - [`Self::prepare_run_context`]: sanitize messages, route tools, build
    ///   system prompt, hydrate todo store, prefetch memory, run preflight
    ///   compress. Pure before any model call.
    /// - the main turn loop (inline; iteration state belongs to the caller).
    /// - the finalize block (writes back the persisted user message, restores
    ///   auth / cost counters, etc.).
    /// Prepare everything needed before the model turn loop.
    ///
    /// Splits the original `run` god-function's setup block into a focused
    /// helper. Does not mutate any iteration state. Returns a `PreparedRun`
    /// bundle the caller destructures.
    async fn prepare_run_context(
        &self,
        mut messages: Vec<Message>,
        tools: Option<&Vec<ToolSchema>>,
    ) -> PreparedRun {
        let mut ctx = ContextManager::for_model_with_providers(
            &self.config.model,
            Some(&self.config.llm_providers),
        );

        // Sanitize surrogates and strip prior budget warnings from history
        for msg in messages.iter_mut() {
            if let Some(ref mut c) = msg.content {
                *c = sanitize_surrogates(c).into_owned();
            }
        }
        strip_budget_warnings_from_messages(&mut messages);

        let task_hint = messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermix_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        // Determine which tools to expose (Phase 7 H1: route through tool_router
        // when configured; otherwise send the full registry as before).
        let tool_schemas: Vec<ToolSchema> = match tools {
            Some(t) => self.route_tools(t.clone(), &messages),
            None => {
                let all = self.tool_registry.schemas();
                self.route_tools(all, &messages)
            }
        };
        // Log the *resolved* tool count (after routing), not the raw
        // parameter. The `tool_count` on the span is the caller's
        // argument and is zero when the caller passes `None`. Was
        // previously open-coded in `run_stream` only; centralizing
        // here so both `run` and `run_stream` emit the same diagnostic.
        if tool_schemas.is_empty() {
            tracing::warn!(
                registry_size = self.tool_registry.schemas().len(),
                "No tool schemas after routing — LLM will not see any tools"
            );
        } else {
            tracing::info!(
                resolved = tool_schemas.len(),
                "Tool schemas resolved for this turn"
            );
        }

        // Build and inject system prompt (or reuse SQLite-cached prompt for session continuity)
        let (system_content, restored_system) =
            self.resolve_initial_system_prompt(&task_hint, &tool_schemas);
        ctx.add_message(Message::system(&system_content));

        let mut session_started_hooks_fired = false;
        if !restored_system {
            let hook_ctx = serde_json::json!({
                "session_id": self.config.session_id,
                "model": self.config.model,
            });
            let _results = self.invoke_hook(HookType::OnSessionStart, &hook_ctx);
            self.inject_hook_context(&_results, &mut ctx);
            session_started_hooks_fired = true;
        }

        // Add initial messages
        for msg in messages {
            ctx.add_message(msg);
        }
        self.hydrate_todo_store(&ctx);

        let persist_user_idx = if self.config.persist_user_message.is_some() {
            ctx.get_messages()
                .iter()
                .enumerate()
                .rfind(|(_, m)| m.role == MessageRole::User)
                .map(|(i, _)| i)
        } else {
            None
        };
        #[allow(unused_mut)] // bound mut so the caller can `+=` after destructuring
        let mut codex_ack_continuations: u32 = 0;
        // Counts how many times the autonomous loop has re-driven the LLM after
        // it produced plain text without tool calls. Capped at
        // `default_max_autonomous_nudges()` to prevent infinite loops when the
        // model genuinely cannot make further progress.
        #[allow(unused_mut)] // bound mut so the caller can `+=` after destructuring
        let mut autonomous_nudge_count: u32 = 0;

        let mut review_memory_at_end = false;
        if self.config.memory_nudge_interval > 0
            && self.tool_registry.names().iter().any(|n| n == "memory")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                c.turns_since_memory = c.turns_since_memory.saturating_add(1);
                if c.turns_since_memory >= self.config.memory_nudge_interval {
                    review_memory_at_end = true;
                    c.turns_since_memory = 0;
                }
            }
        }

        // Memory prefetch for first user message
        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let first_user = ctx
            .get_messages()
            .iter()
            .rfind(|m| matches!(m.role, hermix_core::MessageRole::User))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();
        let mem_ctx_raw = self.memory_prefetch(&first_user, session_id).await;
        if !mem_ctx_raw.is_empty() {
            ctx.add_message(Message::system(&mem_ctx_raw));
        }

        // Phase 18 (BaiLongma gatherer): pre-fetch file context referenced
        // in the user message so the LLM doesn't need to call read_file
        // on the first turn. Best-effort; failures are silently ignored.
        if !first_user.is_empty() {
            let gathered = crate::gatherer::gather_context(
                &first_user,
                self.memory_router.as_ref(),
                None,
            )
            .await;
            if let Some(gather_msg) = crate::gatherer::format_gathered_context(&gathered) {
                ctx.add_message(gather_msg);
            }
        }

        if self.config.preflight_context_compress {
            self.auto_compress_if_over_threshold(&mut ctx).await;
        }

        PreparedRun {
            ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            codex_ack_continuations,
            autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
        }
    }

    pub async fn run(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
    ) -> Result<AgentResult, AgentError> {
        // ---- Step 1: Prepare the run context ----
        // Sanitize messages, route tools, build the system prompt, hydrate the
        // todo store, prefetch memory, and run the optional preflight compress.
        // All of this is the "before any model call" work and is a pure
        // function of the inputs (no streaming, no tool calls yet).
        #[allow(unused_mut)] // codex_ack_continuations is read-only in run() today but mut is fine for future edits
        let PreparedRun {
            mut ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            mut codex_ack_continuations,
            mut autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
        } = self.prepare_run_context(messages, tools.as_ref()).await;

        let session_id = self.config.session_id.as_deref().unwrap_or("");
        let mut tool_errors: Vec<hermix_core::ToolErrorRecord> = Vec::new();

        // Phase 1.1 (2026-07-03) introduced `state` as a bridge — every
        // loop-local mutable would eventually live in `state` so a future
        // `run_shared_loop(state, llm_caller)` helper could take only
        // `&mut LoopState` instead of the original 11+ locals.
        //
        // Phase 5 (2026-07-03..2026-07-04) collapsed the bridge to its
        // minimum: 4 manager structs (`cost_guard` / `retry_counters` /
        // `context_pressure` / `timings` / `message_cache`) own the
        // in-place-mutated fields, and `total_turns` is the only scalar
        // still aliased to a local. The other 13 locals were either
        // dropped or replaced with `record_*` / `bump_*` / `store_*`
        // methods on the inner structs.
        let mut state = LoopState::default();
        // `total_turns` keeps the local-alias pattern because the rest
        // of the loop body reads it as a scalar (the hot path is
        // `if total_turns >= self.config.max_turns` etc.). The struct
        // field `state.total_turns` is still written at the end of
        // each iteration so the future shared loop body can read it
        // without taking 11 more arguments.
        let mut total_turns: u32 = state.total_turns;

        loop {
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            // `/steer` pre-API drain — inject into last tool result before the next LLM call.
            self.pre_api_apply_pending_steer(&mut ctx);

            if total_turns >= self.config.max_turns {
                tracing::warn!(
                    "Max turns ({}) exceeded, requesting final summary",
                    self.config.max_turns
                );
                let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                if let Some(msg) = summary_msg {
                    ctx.add_message(msg);
                }
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    false,
                    None,
                ));
            }

            total_turns += 1;
            tracing::debug!("Agent turn {}", total_turns);

            // Refresh oauth-backed runtime credentials before routing/provider selection.
            self.oauth_refresh
                .refresh_if_needed(&self.auth_tokens_path(), &self.config.runtime_providers)
                .await;

            // Skill nudge counter — Python `run_agent.py`: increment at the start of each inner API iteration.
            if self.config.skill_creation_nudge_interval > 0
                && self
                    .tool_registry
                    .names()
                    .iter()
                    .any(|n| n == "skill_manage")
            {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    c.iters_since_skill = c.iters_since_skill.saturating_add(1);
                }
            }

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1).is_multiple_of(self.config.checkpoint_interval_turns)
            {
                // Phase 5 (2026-07-04): store checkpoint directly on
                // the `state.message_cache` (no more local alias).
                state.message_cache.store_checkpoint(ctx.get_messages().to_vec());
            }

            // Notify memory + plugins of new turn
            self.memory_on_turn_start(total_turns, "");

            // Memory review hint: every 20 turns, remind LLM to consolidate.
            if total_turns > 0 && total_turns.is_multiple_of(20) {
                let review_hint = "Hint: Review and consolidate your recent memory entries. \
                    Remove redundant or outdated information using the memory tool.";
                ctx.add_message(Message::system(review_hint));
            }

            // Memory sync at flush interval
            if total_turns.is_multiple_of(self.config.memory_flush_interval) && total_turns > 0 {
                let msgs = ctx.get_messages();
                let (u, a) = extract_last_user_assistant(msgs);
                self.memory_sync(&u, &a, session_id);
            }
            // Phase 18 (架构-1 + 架构-2): background snapshot + cap.
            self.maybe_freeze_snapshot(session_id, total_turns);
            self.maybe_enforce_frame_cap(total_turns);

            // --- Pre-LLM hook ---
            let turn_runtime_route = state.cost_guard.forced_runtime_route
                .clone()
                .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
            let active_model = turn_runtime_route
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or(self.config.model.as_str());
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- Prefetch: identify user intent, run safe tools, inject results as system prompt ---
            if let Some(ref prefetcher) = self.prefetcher {
                let tool_names: Vec<String> = tool_schemas.iter().map(|t| t.name.clone()).collect();
                let user_text: String = ctx
                    .get_messages()
                    .iter()
                    .filter(|m| m.role == MessageRole::User)
                    .filter_map(|m| m.content.as_deref())
                    .collect::<Vec<_>>()
                    .join("\n");
                let recent_msgs: Vec<Message> = ctx
                    .get_messages()
                    .iter()
                    .cloned()
                    .rev()
                    .take(10)
                    .collect();
                let tc = hermix_intelligence::TurnContext::from_user_message(user_text.clone())
                    .with_recent_messages(recent_msgs);
                if let Some(ref outcome) = prefetcher.run(&tc, &tool_names).await {
                    let block = outcome.to_system_prompt_block();
                    if !block.is_empty() {
                        ctx.add_message(Message::system(&block));
                        tracing::trace!(intent = ?outcome.intent, confidence = outcome.confidence, "prefetch result injected as system prompt block");
                    }
                    // H3: forward prefetch intent to UI layer
                    if let Some(ref on_prefetch_intent) = self.callbacks.on_prefetch_intent {
                        let json = serde_json::to_string(&json!({
                            "intent": serde_json::to_value(outcome.intent.clone()).unwrap_or_default(),
                            "confidence": outcome.confidence,
                            "results": outcome.results.iter().map(|r| json!({
                                "tool": &r.tool,
                                "args": &r.args,
                                "output": &r.output,
                                "errored": r.errored,
                                "duration_ms": r.duration_ms,
                            })).collect::<Value>(),
                        }));
                        if let Ok(ref payload) = json {
                            on_prefetch_intent(payload);
                        }
                    }
                }

                // L4: dual-layer thinking — pick L1 (Quick) or L2 (Deep)
                // for this turn. The mode is informed by the prefetch
                // outcome (intent) and the user text. We only emit a
                // tracing event + ACUI directive here; the actual prompt
                // branching (when to use L1 vs L2 prefix) is the
                // responsibility of the prompt builder that consumes
                // this decision.
                let thinking_decision = {
                    use hermix_intelligence::{select_mode, ThinkingConfig};
                    let cfg = ThinkingConfig::default();
                    let history_len = ctx.get_messages().len();
                    select_mode(user_text.as_str(), history_len, &cfg)
                };
                tracing::trace!(
                    mode = thinking_decision.mode.as_str(),
                    reason = thinking_decision.reason.as_str(),
                    confidence = thinking_decision.confidence,
                    "L4 thinking mode selected for this turn"
                );
                if let Some(ref on_acui) = self.callbacks.on_acui_directive {
                    let directive = hermix_core::AcuiDirective::Show(
                        hermix_core::AcuiShow::new(
                            format!("thinking-mode-{}", total_turns),
                            "thinking_mode",
                            serde_json::json!({
                                "mode": thinking_decision.mode.as_str(),
                                "reason": thinking_decision.reason.as_str(),
                                "confidence": thinking_decision.confidence,
                            }),
                        ),
                    );
                    if let Ok(json) = serde_json::to_string(&directive) {
                        on_acui(&json);
                    }
                }
            }

            // --- LLM API call with transport retry + semantic empty/thinking recovery (Python parity) ---
            let api_start = Instant::now();
            let mut inner_empty = 0u32;
            let mut inner_thinking = 0u32;
            let mut turn_usage_acc: Option<UsageStats> = None;
            let response = loop {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    return Ok(self.graceful_interrupt_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                    ));
                }
                let r = match self
                    .call_llm_with_retry(&ctx, &tool_schemas, turn_runtime_route.as_ref())
                    .await
                {
                    Ok(r) => r,
                    Err(AgentError::Interrupted { .. }) => {
                        return Ok(self.graceful_interrupt_result(
                            &ctx,
                            total_turns,
                            &tool_errors,
                            state.cost_guard.accumulated_usage.clone(),
                            state.cost_guard.session_cost_usd,
                            session_started_hooks_fired,
                            persist_user_idx,
                        ));
                    }
                    Err(e) => return Err(e),
                };
                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                // Phase 4.2 (2026-07-03): ~40 lines of has_tools /
                // visible_text / reasoning-retry / empty-retry /
                // exhausted classification now lives in
                // `classify_post_llm_response` (Phase 4.1). The
                // per-attempt state (`inner_thinking` / `inner_empty`)
                // is owned by the loop and threaded through.
                match self.classify_post_llm_response(
                    r,
                    &mut inner_thinking,
                    &mut inner_empty,
                    &mut ctx,
                ) {
                    PostLlmAction::Break(resp) => break resp,
                    PostLlmAction::RetryReasoning | PostLlmAction::RetryEmpty => continue,
                }
            };
            let api_elapsed = api_start.elapsed().as_millis() as u64;
            // Phase 5 (2026-07-04): record timings on `state.timings`
            // directly (saturating-add inside) instead of accumulating
            // to a local and bridging back via the now-removed
            // `publish_from` helper.
            state.timings.record_api(api_elapsed);

            // Phase 1.3b (2026-07-03): post-LLM hook + usage accumulate +
            // cost guard extracted into `process_post_response`. ~60 lines
            // of duplication with `run_stream()` collapsed into one helper
            // call. Only the `CostGuardTripped` variant short-circuits the
            // loop here; `Finalize` (no tool calls) falls through to the
            // existing `decide_post_response_fallback` block below.
            if matches!(
                self.process_post_response(
                    &mut ctx,
                    &response,
                    turn_usage_acc.as_ref(),
                    total_turns,
                    api_elapsed,
                    &mut state.cost_guard,
                    &mut state.message_cache,
                    false, // stream_mode
                )?,
                PostResponseAction::CostGuardTripped
            ) {
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    false,
                    None,
                ));
            }

            let (history_includes_tool, assistant_msg, tool_calls) = self.prepare_assistant_turn(
                &mut ctx,
                &response,
                &mut state.message_cache,
            );

            // Step complete callback
            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            // If no tool calls, the agent is done. The fallback rules
            // (Codex ack continuation / autonomous nudge / patch last
            // assistant content / finalize) used to be open-coded here
            // and duplicated in `run()`. C2 consolidated that logic
            // into `crate::post_response_fallback::decide_post_response_fallback`
            // so both call sites use the same single authority.
            let tool_calls = match tool_calls {
                Some(calls) if !calls.is_empty() => calls,
                _ => {
                    let outcome = crate::post_response_fallback::decide_post_response_fallback(
                        crate::post_response_fallback::PostResponseFallbackInput {
                            assistant_msg: &assistant_msg,
                            api_mode: self.config.api_mode.clone(),
                            has_tool_schemas: !tool_schemas.is_empty(),
                            history_includes_tool,
                            task_hint: &task_hint,
                            codex_ack_continuations,
                            autonomous_mode: self.config.autonomous_mode,
                            autonomous_nudge_count,
                            total_turns,
                            max_turns: self.config.max_turns,
                            max_autonomous_nudges: default_max_autonomous_nudges(),
                            last_content_with_tools: state.message_cache.take_content_with_tools(),
                            assistant_has_visible_text:
                                Self::assistant_visible_text_after_think_blocks,
                        },
                    );
                    codex_ack_continuations = outcome.new_codex_ack_continuations;
                    autonomous_nudge_count = outcome.new_autonomous_nudge_count;
                    if self.apply_fallback_outcome(
                        &mut ctx,
                        outcome.action,
                        false, // stream_mode
                        total_turns,
                        autonomous_nudge_count,
                    ) {
                        continue;
                    }
                    tracing::debug!("No tool calls in response, finishing naturally");
                    // Phase 2.4: terminal-persistence shared with run_stream
                    self.persist_session_terminal(&ctx, session_id, total_turns, review_memory_at_end);
                    return Ok(self.finalize_agent_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        true,
                        false,
                        None,
                    ));
                }
            };

            codex_ack_continuations = 0;

            // Deduplicate tool calls
            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
                self.hydrate_session_search_args(tc);
            }
            let invalid_tool_calls: Vec<String> = tool_calls
                .iter()
                .filter(|tc| self.tool_registry.get(&tc.function.name).is_none())
                .map(|tc| tc.function.name.clone())
                .collect();
            if !invalid_tool_calls.is_empty() {
                let count = state.retry_counters.bump_invalid_tool();
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Invalid tool call detected — retrying ({}/{})",
                        count, self.config.invalid_tool_call_max_retries
                    ),
                );
                let available = self.tool_registry.names().join(", ");
                if count >= self.config.invalid_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Max invalid tool retries reached ({})",
                            self.config.invalid_tool_call_max_retries
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Max invalid tool retries reached ({}). Last invalid tool: {}",
                        self.config.invalid_tool_call_max_retries, invalid_tool_calls[0]
                    )));
                    return Ok(self.finalize_agent_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        false,
                        false,
                    None,
                    ));
                }
                for tc in &tool_calls {
                    let content = if self.tool_registry.get(&tc.function.name).is_none() {
                        format!(
                            "Tool '{}' does not exist. Available tools: {}",
                            tc.function.name, available
                        )
                    } else {
                        "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content, Some(tc.function.name.as_str())));
                }
                continue;
            }
            state.retry_counters.reset_invalid_tool();

            let mut invalid_json_args: Vec<(String, String)> = Vec::new();
            for tc in &mut tool_calls {
                if let Err(e) = Self::normalize_tool_call_arguments(tc) {
                    invalid_json_args.push((tc.function.name.clone(), e));
                }
            }
            if !invalid_json_args.is_empty() {
                let count = state.retry_counters.bump_invalid_json();
                if count < self.config.invalid_tool_json_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Invalid tool JSON arguments — retrying ({}/{})",
                            count, self.config.invalid_tool_json_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ),
                );
                state.retry_counters.reset_invalid_json();
                for tc in &tool_calls {
                    let content = if let Some((_, err)) = invalid_json_args
                        .iter()
                        .find(|(name, _)| name == &tc.function.name)
                    {
                        format!(
                                "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                                err
                            )
                    } else {
                        "Skipped: other tool call in this response had invalid JSON.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content, Some(tc.function.name.as_str())));
                }
                continue;
            }
            state.retry_counters.reset_invalid_json();

            for tc in &tool_calls {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    match tc.function.name.as_str() {
                        "memory" => c.turns_since_memory = 0,
                        "skill_manage" => c.iters_since_skill = 0,
                        _ => {}
                    }
                }
            }

            // Cap concurrent delegate_task calls
            self.cap_delegates(&mut tool_calls);

            // --- Pre-tool hook ---
            //
            // Generate a per-invocation uuid up-front and stash it on
            // a side-table keyed by the LLM-assigned `tc.id`. We need
            // the same id back in the post-tool loop below so callers
            // (e.g. `LocalAgentService`'s `on_tool_complete` closure)
            // can pair start and complete events for the same tool
            // call. The `tc.id` is the only thing that survives both
            // the pre-loop (which sees `tool_calls`) and the
            // post-loop (which sees `results` — keyed by
            // `result.tool_call_id`, identical to `tc.id`).
            let mut tool_call_ids: HashMap<String, String> = HashMap::new();
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    let tool_call_id = uuid::Uuid::new_v4().to_string();
                    tool_call_ids.insert(tc.id.clone(), tool_call_id.clone());
                    cb(&tool_call_id, &tc.function.name, &args);
                }
            }

            // --- Execute tool calls in parallel ---
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }
            let tool_start = Instant::now();
            let results = self
                .execute_tool_calls(
                    &tool_calls,
                    total_turns,
                    self.config
                        .max_cost_usd
                        .map(|limit| (limit - state.cost_guard.session_cost_usd).max(0.0)),
                    &mut tool_errors,
                )
                .await;
            let tool_elapsed = tool_start.elapsed().as_millis() as u64;
            // Phase 5 (2026-07-04): see `record_api` above.
            state.timings.record_tool(tool_elapsed);

            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = state.message_cache.checkpoint().cloned() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // --- Post-tool hook ---
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                let tc_ctx = serde_json::json!({
                    "tool": &tc.function.name,
                    "is_error": res.is_error,
                    "turn": total_turns,
                });
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);

                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    // Fall back to a synthetic id if the pre-loop
                    // somehow didn't seed one (e.g. a tool call
                    // skipped by `cap_delegates` after the
                    // map was built). In practice this branch
                    // is never hit — every pre-loop `tc` gets
                    // a uuid — but keeping the fallback means a
                    // missing map entry doesn't blow up the
                    // dispatch.
                    let tool_call_id = tool_call_ids
                        .get(&tc.id)
                        .cloned()
                        .unwrap_or_else(|| format!("legacy-{}", tc.function.name));
                    cb(&tool_call_id, &tc.function.name, &res.content);
                }
            }

            // --- File-change summary events ---
            // Emit a `file_change` timeline event for tools that mutate
            // the filesystem (write_file, patch, delete). The chat UI
            // renders these as a WorkBuddy-style "文件变更 (N个)" row.
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                if res.is_error {
                    continue;
                }
                if let Some(change) = extract_file_change(&tc.function.name, &tc.function.arguments, &res.content) {
                    if let Some(ref cb) = self.callbacks.on_tool_complete {
                        let synth_id = format!("file_change::{}", change.path);
                        let synth_name = format!("file_change::{}", change.path);
                        let payload = serde_json::json!({
                            "change_type": change.change_type,
                            "file_path": change.path,
                            "file_summary": change.summary,
                        }).to_string();
                        cb(&synth_id, &synth_name, &payload);
                    }
                }
            }

            self.notify_memory_writes(&tool_calls, &results);
            self.notify_delegations(&tool_calls, &results);

            // Enforce budget on tool results
            let mut results = results;
            budget::enforce_budget(&mut results, &self.config.budget);

            if !results.is_empty() {
                let w = budget_pressure_text(
                    total_turns,
                    self.config.max_turns,
                    self.config.budget_caution_threshold,
                    self.config.budget_warning_threshold,
                    self.config.budget_pressure_enabled,
                );
                if let Some(ref text) = w {
                    tracing::info!("{}", text);
                }
                inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
            }

            for result in results {
                // Look up the original tool name (e.g. "terminal")
                // from the matching tool_calls entry so reloaded
                // sessions can rebuild ToolMessageMeta with the real
                // label. Without this the persisted `Message::name`
                // is None and the UI falls back to a generic "tool"
                // pill on reload.
                let tool_name = tool_calls
                    .iter()
                    .find(|tc| tc.id == result.tool_call_id)
                    .map(|tc| tc.function.name.as_str());
                // Truncation is owned by budget::enforce_budget (above),
                // which is now the single char-safe path. See A1 commit.
                ctx.add_message(Message::tool_result(
                    &result.tool_call_id,
                    &result.content,
                    tool_name,
                ));
            }
            self.post_tool_apply_pending_steer(&mut ctx);
            if !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|tc| tc.function.name == "execute_code")
            {
                total_turns = total_turns.saturating_sub(1);
            }
            self.emit_background_review_metrics(total_turns, &ctx);

            let total_tokens = ctx.total_tokens();
            let threshold = ctx.pressure_threshold_tokens();
            if threshold > 0 {
                let progress = total_tokens as f64 / threshold as f64;
                let tier = if progress >= 0.95 {
                    0.95
                } else if progress >= 0.85 {
                    0.85
                } else {
                    0.0
                };
                if state.context_pressure.should_emit_warning(progress, tier) {
                    let message = format!(
                        "Context pressure {:.0}% of compaction threshold ({} / {})",
                        progress * 100.0,
                        format_token_count(total_tokens),
                        format_token_count(threshold)
                    );
                    tracing::warn!("{}", message);
                    self.emit_status("lifecycle", &message);
                }
            }

            self.auto_compress_if_over_threshold(&mut ctx).await;

            // Write the only remaining loop-local back to `state` so the
            // future `run_shared_loop(state, llm_caller)` helper can read
            // it without taking 11+ locals. See the bridge comment at the
            // top of `run()` for the full history of the 14 -> 1
            // writeback shrink.
            state.total_turns = total_turns;
        }
    }

    /// Run the agent loop with streaming.
    ///
    /// Uses the LLM provider's streaming API and invokes `on_chunk` for each
    /// incremental delta. The stream is collected into a complete response
    /// before tool execution proceeds.
    #[instrument(
        name = "run_stream",
        level = "info",
        skip_all,
        fields(
            model = %self.config.model,
            provider = %self.config.provider.as_deref().unwrap_or(""),
            msg_count = messages.len(),
            tool_count = tools.as_ref().map(|t| t.len()).unwrap_or(0),
            total_turns = tracing::field::Empty,
            total_ms = tracing::field::Empty,
        )
    )]
    pub async fn run_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolSchema>>,
        on_chunk: Option<Box<dyn Fn(StreamChunk) + Send + Sync>>,
    ) -> Result<AgentResult, AgentError> {
        let _run_started = Instant::now();
        info!(
            msg_count = messages.len(),
            tool_count = tools.as_ref().map(|t| t.len()).unwrap_or(0),
            "run_stream entered"
        );
        let on_chunk = match on_chunk {
            Some(cb) => cb,
            None => {
                return self.run(messages, tools).await;
            }
        };
        // Track the most recent finish_reason across all LLM calls in this
        // streaming run so we can surface it in AgentResult → WS Done event.
        let mut last_finish_reason: Option<String> = None;
        let stream_mute = Arc::new(AtomicBool::new(false));
        let stream_needs_break = Arc::new(AtomicBool::new(false));
        let raw_emit = on_chunk;
        let mute_for_emit = stream_mute.clone();
        let break_for_emit = stream_needs_break.clone();
        let on_chunk: Box<dyn Fn(StreamChunk) + Send + Sync> =
            Box::new(move |chunk: StreamChunk| {
                let StreamChunk {
                    delta,
                    finish_reason,
                    usage,
                } = chunk;
                if let Some(delta_val) = delta {
                    if let Some(content) = delta_val.content.clone() {
                        if mute_for_emit.load(Ordering::Acquire) {
                            return;
                        }
                        let mut out = content;
                        if break_for_emit.swap(false, Ordering::AcqRel) {
                            out = format!("\n\n{}", out);
                        }
                        raw_emit(StreamChunk {
                            delta: Some(hermix_core::StreamDelta {
                                content: Some(out),
                                tool_calls: delta_val.tool_calls,
                                extra: delta_val.extra,
                            }),
                            finish_reason,
                            usage,
                        });
                        return;
                    }
                    raw_emit(StreamChunk {
                        delta: Some(delta_val),
                        finish_reason,
                        usage,
                    });
                    return;
                }
                raw_emit(StreamChunk {
                    delta: None,
                    finish_reason,
                    usage,
                });
            });

        let mut tool_errors: Vec<hermix_core::ToolErrorRecord> = Vec::new();
        // Phase 3.1 (2026-07-03): `run_stream` setup was an open-coded
        // re-implementation of `prepare_run_context`. The two functions
        // diverge only in their *non-prepare* concerns — `run()` is
        // non-stream, `run_stream()` wraps the chunk callback and
        // tracks `last_finish_reason`. The "before any model call"
        // work (sanitize / strip / task_hint / route_tools /
        // system_prompt / session_start hook / hydrate_todo /
        // persist_user_idx / codex+autonomous counters /
        // review_memory_at_end / memory_prefetch /
        // auto_compress_if_over_threshold) is identical and now
        // lives in one place. The tool-schemas tracing that used to
        // live only in `run_stream` was promoted to
        // `prepare_run_context` so both call sites emit the same
        // diagnostic.
        let PreparedRun {
            mut ctx,
            tool_schemas,
            session_started_hooks_fired,
            persist_user_idx,
            mut codex_ack_continuations,
            mut autonomous_nudge_count,
            review_memory_at_end,
            task_hint,
        } = self.prepare_run_context(messages, tools.as_ref()).await;
        let session_id = self.config.session_id.as_deref().unwrap_or("");

        let mut state = LoopState::default();
        // Phase 5 (2026-07-04): same as in `run()`. Only `total_turns`
        // keeps the local-alias pattern. The two message caches live
        // on `state.message_cache` (mutated in place) and the two
        // timings live on `state.timings` (mutated in place).
        let mut total_turns: u32 = state.total_turns;

        loop {
            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            // `/steer` pre-API drain — inject into last tool result before the next LLM call.
            self.pre_api_apply_pending_steer(&mut ctx);

            if total_turns >= self.config.max_turns {
                tracing::warn!(
                    "Max turns ({}) exceeded, requesting final summary",
                    self.config.max_turns
                );
                let summary_msg = self.handle_max_iterations(&mut ctx).await?;
                if let Some(msg) = summary_msg {
                    ctx.add_message(msg);
                }
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    false,
                    last_finish_reason.clone(),
                ));
            }

            total_turns += 1;

            // Refresh oauth-backed runtime credentials before routing/provider selection.
            self.oauth_refresh
                .refresh_if_needed(&self.auth_tokens_path(), &self.config.runtime_providers)
                .await;

            if self.config.skill_creation_nudge_interval > 0
                && self
                    .tool_registry
                    .names()
                    .iter()
                    .any(|n| n == "skill_manage")
            {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    c.iters_since_skill = c.iters_since_skill.saturating_add(1);
                }
            }

            self.memory_on_turn_start(total_turns, "");

            if self.config.checkpoint_interval_turns > 0
                && (total_turns - 1).is_multiple_of(self.config.checkpoint_interval_turns)
            {
                // Phase 5 (2026-07-04): store checkpoint directly on
                // the `state.message_cache` (no more local alias).
                state.message_cache.store_checkpoint(ctx.get_messages().to_vec());
            }

            if total_turns.is_multiple_of(self.config.memory_flush_interval) && total_turns > 0 {
                let (u, a) = extract_last_user_assistant(ctx.get_messages());
                self.memory_sync(&u, &a, session_id);
            }
            // Phase 18 (架构-1 + 架构-2): background snapshot + cap.
            self.maybe_freeze_snapshot(session_id, total_turns);
            self.maybe_enforce_frame_cap(total_turns);

            // Pre-LLM hook
            let turn_runtime_route = state.cost_guard.forced_runtime_route
                .clone()
                .or_else(|| self.resolve_smart_runtime_route(ctx.get_messages()));
            let active_model = turn_runtime_route
                .as_ref()
                .map(|r| r.model.as_str())
                .unwrap_or(self.config.model.as_str());
            let hook_ctx = serde_json::json!({"turn": total_turns, "model": active_model});
            let pre_results = self.invoke_hook(HookType::PreLlmCall, &hook_ctx);
            self.inject_hook_context(&pre_results, &mut ctx);

            // --- Streaming first attempt + same D-step semantic recovery as `run()` (retries use non-stream) ---
            let api_start = Instant::now();
            let mut inner_empty = 0u32;
            let mut inner_thinking = 0u32;
            let mut turn_usage_acc: Option<UsageStats> = None;
            let mut inner_attempt: u32 = 0;
            let response = loop {
                if self.interrupt.take_interrupt_graceful().is_some() {
                    return Ok(self.graceful_interrupt_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                    ));
                }
                let r = if inner_attempt == 0 {
                    let pre_api_ctx = serde_json::json!({
                        "attempt": inner_attempt,
                        "model": active_model,
                        "stream": true,
                        "route_label": turn_runtime_route
                            .as_ref()
                            .and_then(|r| r.route_label.as_deref()),
                    });
                    let _ = self.invoke_hook(HookType::PreApiRequest, &pre_api_ctx);
                    let stream_collect = self
                        .collect_stream_llm_response(
                            &ctx,
                            &tool_schemas,
                            turn_runtime_route.as_ref(),
                            active_model,
                            &*on_chunk,
                        )
                        .await;
                    let post_api_ctx = match &stream_collect {
                        Ok(StreamCollectOutcome::Complete(resp)) => {
                            // Remember the most recent finish_reason so the
                            // final AgentResult can surface it to the WS client.
                            last_finish_reason = resp.finish_reason.clone();
                            serde_json::json!({
                                "attempt": inner_attempt,
                                "model": active_model,
                                "stream": true,
                                "ok": true,
                                "finish_reason": resp.finish_reason,
                            })
                        },
                        Ok(StreamCollectOutcome::Interrupted(_)) => serde_json::json!({
                            "attempt": inner_attempt,
                            "model": active_model,
                            "stream": true,
                            "ok": false,
                            "interrupted": true,
                        }),
                        Err(err) => serde_json::json!({
                            "attempt": inner_attempt,
                            "model": active_model,
                            "stream": true,
                            "ok": false,
                            "error": err.to_string(),
                        }),
                    };
                    let _ = self.invoke_hook(HookType::PostApiRequest, &post_api_ctx);
                    match stream_collect? {
                        StreamCollectOutcome::Complete(resp) => resp,
                        StreamCollectOutcome::Interrupted(partial) => {
                            state
                                .cost_guard
                                .record_usage(partial.usage.as_ref(), partial.model.as_str(), &self.config);
                            ctx.add_message(partial.message);
                            return Ok(self.graceful_interrupt_result(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                state.cost_guard.accumulated_usage.clone(),
                                state.cost_guard.session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                            ));
                        }
                    }
                } else {
                    match self
                        .call_llm_with_retry(&ctx, &tool_schemas, turn_runtime_route.as_ref())
                        .await
                    {
                        Ok(r) => r,
                        Err(AgentError::Interrupted { .. }) => {
                            return Ok(self.graceful_interrupt_result(
                                &ctx,
                                total_turns,
                                &tool_errors,
                                state.cost_guard.accumulated_usage.clone(),
                                state.cost_guard.session_cost_usd,
                                session_started_hooks_fired,
                                persist_user_idx,
                            ));
                        }
                        Err(e) => return Err(e),
                    }
                };
                inner_attempt = inner_attempt.saturating_add(1);

                if let Some(ref u) = r.usage {
                    turn_usage_acc = Some(merge_usage(turn_usage_acc, u));
                }

                // Phase 4.2 (2026-07-03): mirrors the run() side —
                // ~40 lines of has_tools / visible_text /
                // reasoning-retry / empty-retry / exhausted
                // classification collapse into a single `match` on
                // `classify_post_llm_response` (Phase 4.1). The
                // `inner_attempt` counter (stream-only) is bumped
                // outside this match; per-attempt state is owned by
                // the loop.
                match self.classify_post_llm_response(
                    r,
                    &mut inner_thinking,
                    &mut inner_empty,
                    &mut ctx,
                ) {
                    PostLlmAction::Break(resp) => break resp,
                    PostLlmAction::RetryReasoning | PostLlmAction::RetryEmpty => continue,
                }
            };
            let api_elapsed_ms = api_start.elapsed().as_millis() as u64;

            // Phase 1.3c (2026-07-03): mirrors 1.3b on the run_stream()
            // side. Post-LLM hook + usage accumulate + cost guard move
            // into `process_post_response`. The stream-specific
            // `llm_response_received` info! line is now emitted inside
            // the helper when `stream_mode = true`. The
            // `last_finish_reason` was already updated inside the
            // stream retry loop (line ~5640) to the final response's
            // finish_reason — no need to refresh it here.
            if matches!(
                self.process_post_response(
                    &mut ctx,
                    &response,
                    turn_usage_acc.as_ref(),
                    total_turns,
                    api_elapsed_ms,
                    &mut state.cost_guard,
                    &mut state.message_cache,
                    true, // stream_mode
                )?,
                PostResponseAction::CostGuardTripped
            ) {
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    false,
                    false,
                    last_finish_reason.clone(),
                ));
            }

            let (history_includes_tool, assistant_msg, tool_calls) = self.prepare_assistant_turn(
                &mut ctx,
                &response,
                &mut state.message_cache,
            );
            if response.finish_reason.as_deref() == Some("length")
                && assistant_msg
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty())
            {
                let count = state.retry_counters.bump_truncated_tool();
                if count <= self.config.truncated_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Truncated tool arguments — retrying ({}/{})",
                            count, self.config.truncated_tool_call_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
            }
            state.retry_counters.reset_truncated_tool();

            if let Some(ref cb) = self.callbacks.on_step_complete {
                cb(total_turns);
            }

            let tool_calls: Vec<ToolCall> = tool_calls
                .unwrap_or_default()
                .into_iter()
                .filter(|tc| !tc.function.name.is_empty())
                .collect();

            if tool_calls.is_empty() {
                // C2 refactor: protocol fallback single point. The
                // Codex-ack / autonomous-nudge / patch-last-assistant /
                // finalize decisions used to be open-coded here and
                // duplicated in `run_blocking`. Now both call sites
                // delegate to `decide_post_response_fallback`.
                let outcome = crate::post_response_fallback::decide_post_response_fallback(
                    crate::post_response_fallback::PostResponseFallbackInput {
                        assistant_msg: &assistant_msg,
                        api_mode: self.config.api_mode.clone(),
                        has_tool_schemas: !tool_schemas.is_empty(),
                        history_includes_tool,
                        task_hint: &task_hint,
                        codex_ack_continuations,
                        autonomous_mode: self.config.autonomous_mode,
                        autonomous_nudge_count,
                        total_turns,
                        max_turns: self.config.max_turns,
                        max_autonomous_nudges: default_max_autonomous_nudges(),
                        last_content_with_tools: state.message_cache.take_content_with_tools(),
                        assistant_has_visible_text:
                            Self::assistant_visible_text_after_think_blocks,
                    },
                );
                codex_ack_continuations = outcome.new_codex_ack_continuations;
                autonomous_nudge_count = outcome.new_autonomous_nudge_count;
                if self.apply_fallback_outcome(
                    &mut ctx,
                    outcome.action,
                    true, // stream_mode
                    total_turns,
                    autonomous_nudge_count,
                ) {
                    continue;
                }
                // Phase 2.4: terminal-persistence shared with run()
                self.persist_session_terminal(&ctx, session_id, total_turns, review_memory_at_end);
                Self::emit_mute_transition(&on_chunk, &stream_mute, false);
                return Ok(self.finalize_agent_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                    true,
                    false,
                    last_finish_reason.clone(),
                ));
            }

            codex_ack_continuations = 0;

            let mut tool_calls = Self::deduplicate_tool_calls(&tool_calls);
            for tc in &mut tool_calls {
                self.repair_tool_call(tc);
                self.hydrate_session_search_args(tc);
            }
            let all_housekeeping = tool_calls.iter().all(|tc| {
                matches!(
                    tc.function.name.as_str(),
                    "memory" | "todo" | "skill_manage" | "session_search"
                )
            });
            let should_mute_post =
                all_housekeeping && Self::assistant_visible_text_after_think_blocks(&assistant_msg);
            Self::emit_mute_transition(&on_chunk, &stream_mute, should_mute_post);

            let invalid_tool_calls: Vec<String> = tool_calls
                .iter()
                .filter(|tc| self.tool_registry.get(&tc.function.name).is_none())
                .map(|tc| tc.function.name.clone())
                .collect();
            if !invalid_tool_calls.is_empty() {
                let count = state.retry_counters.bump_invalid_tool();
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Invalid tool call detected — retrying ({}/{})",
                        count, self.config.invalid_tool_call_max_retries
                    ),
                );
                let available = self.tool_registry.names().join(", ");
                if count >= self.config.invalid_tool_call_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Max invalid tool retries reached ({})",
                            self.config.invalid_tool_call_max_retries
                        ),
                    );
                    ctx.add_message(Message::system(format!(
                        "Max invalid tool retries reached ({}). Last invalid tool: {}",
                        self.config.invalid_tool_call_max_retries, invalid_tool_calls[0]
                    )));
                    return Ok(self.finalize_agent_result(
                        &ctx,
                        total_turns,
                        &tool_errors,
                        state.cost_guard.accumulated_usage.clone(),
                        state.cost_guard.session_cost_usd,
                        session_started_hooks_fired,
                        persist_user_idx,
                        false,
                        false,
                    last_finish_reason.clone(),
                    ));
                }
                for tc in &tool_calls {
                    let content = if self.tool_registry.get(&tc.function.name).is_none() {
                        format!(
                            "Tool '{}' does not exist. Available tools: {}",
                            tc.function.name, available
                        )
                    } else {
                        "Skipped: another tool call in this turn used an invalid name. Please retry this tool call.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content, Some(tc.function.name.as_str())));
                }
                continue;
            }
            state.retry_counters.reset_invalid_tool();

            let mut invalid_json_args: Vec<(String, String)> = Vec::new();
            for tc in &mut tool_calls {
                if let Err(e) = Self::normalize_tool_call_arguments(tc) {
                    invalid_json_args.push((tc.function.name.clone(), e));
                }
            }
            if !invalid_json_args.is_empty() {
                let count = state.retry_counters.bump_invalid_json();
                if count < self.config.invalid_tool_json_max_retries {
                    self.emit_status(
                        "lifecycle",
                        &format!(
                            "Invalid tool JSON arguments — retrying ({}/{})",
                            count, self.config.invalid_tool_json_max_retries
                        ),
                    );
                    let _ = ctx.get_messages_mut().pop();
                    continue;
                }
                self.emit_status(
                    "lifecycle",
                    &format!(
                        "Max invalid JSON retries reached ({}); returning tool errors",
                        self.config.invalid_tool_json_max_retries
                    ),
                );
                state.retry_counters.reset_invalid_json();
                for tc in &tool_calls {
                    let content = if let Some((_, err)) = invalid_json_args
                        .iter()
                        .find(|(name, _)| name == &tc.function.name)
                    {
                        format!(
                                "Error: Invalid JSON arguments. {}. For tools with no required parameters, use an empty object: {{}}. Please retry with valid JSON.",
                                err
                            )
                    } else {
                        "Skipped: other tool call in this response had invalid JSON.".to_string()
                    };
                    ctx.add_message(Message::tool_result(tc.id.clone(), content, Some(tc.function.name.as_str())));
                }
                continue;
            }
            state.retry_counters.reset_invalid_json();
            for tc in &tool_calls {
                if let Ok(mut c) = self.evolution_counters.lock() {
                    match tc.function.name.as_str() {
                        "memory" => c.turns_since_memory = 0,
                        "skill_manage" => c.iters_since_skill = 0,
                        _ => {}
                    }
                }
            }
            self.cap_delegates(&mut tool_calls);

            // Pre-tool hooks + callbacks. Same `tc.id → uuid` map
            // pattern as the primary dispatch path (line ~3460) so
            // callers can pair start/complete events. The two
            // dispatch paths produce different `results` types
            // (synchronous gather here vs. parallel `execute_tool_calls`
            // in the primary path) but both end up keyed by
            // `result.tool_call_id == tc.id`.
            let mut tool_call_ids: HashMap<String, String> = HashMap::new();
            for tc in &tool_calls {
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "turn": total_turns});
                self.invoke_hook(HookType::PreToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_start {
                    let args: Value =
                        serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null);
                    let tool_call_id = uuid::Uuid::new_v4().to_string();
                    tool_call_ids.insert(tc.id.clone(), tool_call_id.clone());
                    cb(&tool_call_id, &tc.function.name, &args);
                }
            }

            if self.interrupt.take_interrupt_graceful().is_some() {
                return Ok(self.graceful_interrupt_result(
                    &ctx,
                    total_turns,
                    &tool_errors,
                    state.cost_guard.accumulated_usage.clone(),
                    state.cost_guard.session_cost_usd,
                    session_started_hooks_fired,
                    persist_user_idx,
                ));
            }

            let mut results = self
                .execute_tool_calls(
                    &tool_calls,
                    total_turns,
                    self.config
                        .max_cost_usd
                        .map(|limit| (limit - state.cost_guard.session_cost_usd).max(0.0)),
                    &mut tool_errors,
                )
                .await;

            let turn_tool_error_count = results.iter().filter(|r| r.is_error).count() as u32;
            if self.config.rollback_on_tool_error_threshold > 0
                && turn_tool_error_count >= self.config.rollback_on_tool_error_threshold
            {
                if let Some(snapshot) = state.message_cache.checkpoint().cloned() {
                    *ctx.get_messages_mut() = snapshot;
                    ctx.add_message(Message::system(format!(
                        "Auto-rollback: {} tool call(s) failed in one turn. Restored latest checkpoint and continuing.",
                        turn_tool_error_count
                    )));
                    continue;
                }
            }

            // Post-tool hooks + callbacks. Look up the uuid we
            // stashed in the pre-loop keyed by the same `tc.id` so
            // callers can correlate start/complete events.
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                let tc_ctx = serde_json::json!({"tool": &tc.function.name, "is_error": res.is_error, "turn": total_turns});
                self.invoke_hook(HookType::PostToolCall, &tc_ctx);
                if let Some(ref cb) = self.callbacks.on_tool_complete {
                    let tool_call_id = tool_call_ids
                        .get(&tc.id)
                        .cloned()
                        .unwrap_or_else(|| format!("legacy-{}", tc.function.name));
                    cb(&tool_call_id, &tc.function.name, &res.content);
                }
            }

            // File-change summary events (mirrors the first on_tool_complete block).
            for res in &results {
                let Some(tc) = tool_calls.iter().find(|tc| tc.id == res.tool_call_id) else {
                    continue;
                };
                if res.is_error {
                    continue;
                }
                if let Some(change) = extract_file_change(&tc.function.name, &tc.function.arguments, &res.content) {
                    if let Some(ref cb) = self.callbacks.on_tool_complete {
                        let synth_id = format!("file_change::{}", change.path);
                        let synth_name = format!("file_change::{}", change.path);
                        let payload = serde_json::json!({
                            "change_type": change.change_type,
                            "file_path": change.path,
                            "file_summary": change.summary,
                        }).to_string();
                        cb(&synth_id, &synth_name, &payload);
                    }
                }
            }

            self.notify_memory_writes(&tool_calls, &results);
            self.notify_delegations(&tool_calls, &results);

            budget::enforce_budget(&mut results, &self.config.budget);

            if !results.is_empty() {
                let w = budget_pressure_text(
                    total_turns,
                    self.config.max_turns,
                    self.config.budget_caution_threshold,
                    self.config.budget_warning_threshold,
                    self.config.budget_pressure_enabled,
                );
                if let Some(ref text) = w {
                    tracing::info!("{}", text);
                }
                inject_budget_pressure_into_last_tool_result(&mut results, w.as_deref());
            }

            for result in results {
                // Look up the original tool name (e.g. "terminal")
                // from the matching tool_calls entry so reloaded
                // sessions can rebuild ToolMessageMeta with the real
                // label. Without this the persisted `Message::name`
                // is None and the UI falls back to a generic "tool"
                // pill on reload.
                let tool_name = tool_calls
                    .iter()
                    .find(|tc| tc.id == result.tool_call_id)
                    .map(|tc| tc.function.name.as_str());
                // Truncation is owned by budget::enforce_budget (above),
                // which is now the single char-safe path. See A1 commit.
                ctx.add_message(Message::tool_result(
                    &result.tool_call_id,
                    &result.content,
                    tool_name,
                ));
            }
            self.post_tool_apply_pending_steer(&mut ctx);
            if !tool_calls.is_empty()
                && tool_calls
                    .iter()
                    .all(|tc| tc.function.name == "execute_code")
            {
                total_turns = total_turns.saturating_sub(1);
            }
            stream_needs_break.store(true, Ordering::Release);
            on_chunk(StreamChunk {
                delta: Some(hermix_core::StreamDelta {
                    content: None,
                    tool_calls: None,
                    extra: Some(serde_json::json!({"control": "stream_break"})),
                }),
                finish_reason: None,
                usage: None,
            });
            self.emit_background_review_metrics(total_turns, &ctx);

            let total_tokens = ctx.total_tokens();
            let threshold = ctx.pressure_threshold_tokens();
            if threshold > 0 {
                let progress = total_tokens as f64 / threshold as f64;
                let tier = if progress >= 0.95 {
                    0.95
                } else if progress >= 0.85 {
                    0.85
                } else {
                    0.0
                };
                if state.context_pressure.should_emit_warning(progress, tier) {
                    let message = format!(
                        "Context pressure {:.0}% of compaction threshold ({} / {})",
                        progress * 100.0,
                        format_token_count(total_tokens),
                        format_token_count(threshold)
                    );
                    tracing::warn!("{}", message);
                    self.emit_status("lifecycle", &message);
                }
            }

            self.auto_compress_if_over_threshold(&mut ctx).await;

            // Mirror of the writeback at the end of `run()`: the only
            // loop-local still bridged into `state` is `total_turns`.
            // See the bridge comment at the top of `run()` for history.
            state.total_turns = total_turns;
        }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Remove duplicate tool calls that share the same function name and arguments.
    fn deduplicate_tool_calls(calls: &[ToolCall]) -> Vec<ToolCall> {
        crate::tool_dispatch::deduplicate_tool_calls(calls)
    }

    /// Try to repair an unknown tool name via case-insensitive or substring matching.
    /// Returns `true` if the tool call was repaired.
    fn repair_tool_call(&self, tc: &mut ToolCall) -> bool {
        crate::tool_dispatch::repair_tool_call(self, tc)
    }

    /// Inject current session id into `session_search` calls when absent.
    fn hydrate_session_search_args(&self, tc: &mut ToolCall) {
        crate::tool_dispatch::hydrate_session_search_args(self, tc)
    }

    fn turn_route_cost_guard(&self, model: String) -> TurnRuntimeRoute {
        crate::cost_routing::turn_route_cost_guard(self, model)
    }

    fn resolve_smart_runtime_route(&self, messages: &[Message]) -> Option<TurnRuntimeRoute> {
        crate::cost_routing::resolve_smart_runtime_route(self, messages)
    }

    fn resolve_cost_degrade_model(&self) -> Option<String> {
        crate::cost_routing::resolve_cost_degrade_model(self)
    }

    /// Ask the LLM for a final summary when the turn budget is exhausted.
    async fn handle_max_iterations(
        &self,
        ctx: &mut ContextManager,
    ) -> Result<Option<Message>, AgentError> {
        ctx.add_message(Message::system(
            "[SYSTEM] Maximum conversation turns reached. Please provide a brief summary of \
             what was accomplished and any remaining tasks.",
        ));
        let response = self
            .llm_provider
            .chat_completion(
                ctx.get_messages(),
                &[],
                self.config.max_tokens,
                self.config.temperature,
                Some(self.config.model.as_str()),
                self.extra_body_for_api_mode(&self.config.api_mode).as_ref(),
            )
            .await
            .map_err(|e| AgentError::LlmApi(e.to_string()))?;
        Ok(Some(response.message))
    }

    /// Build a per-tool hint that we append to a user-denial reason so the
    /// LLM, on its next turn, has a concrete suggestion for *which* tool
    /// would have been more appropriate.
    ///
    /// ## Why this exists
    ///
    /// Regression (2026-06-14): user said "看下 D:\data 下有啥". The model
    /// chose `computer_use` with `action=screenshot`. The user denied.
    /// The model then claimed "I have no terminal / list_files / read_file
    /// tool" — even though all three are registered and pinned to the top
    /// of the `<tools>` block.
    ///
    /// Two layers of defense had been added (descriptions + system-prompt
    /// reminder) but neither was anchored to the deny path. By the time
    /// the user hits Deny, the model has already committed to its choice;
    /// a plain "User denied the tool call." gives it no signal to re-pick.
    /// The hint below is the *third* defense: when the user rejects,
    /// surface the right next move in the very tool-result message the
    /// model will read.
    ///
    /// ## Design constraints
    ///
    /// - **Short** — the tool-result string is fed back to the LLM, and
    ///   every extra token here competes with the user's original prompt
    ///   in the next turn. We aim for one sentence, not a paragraph.
    /// - **Tool-specific** — generic "consider another tool" hints are
    ///   too vague. We map the most-mis-picked tools to the right
    ///   replacement so the LLM has a concrete noun to call.
    /// Resolve the pre-tool-dispatch hook for every tool call up front.
    ///
    /// When `AgentCallbacks::on_pre_tool_dispatch` is set, this invokes the
    /// hook once per tool call. `Allow` and `Deny` decisions resolve
    /// synchronously; `AwaitApproval` blocks on a `oneshot::Receiver` until
    /// the upstream layer (e.g. `LocalAgentService`) sends a verdict. The
    /// returned map is keyed by `ToolCall::id` and consumed by both the
    /// orchestrated `delegate_task` path and the `JoinSet` path.
    async fn resolve_pre_dispatch(
        &self,
        tool_calls: &[ToolCall],
    ) -> HashMap<String, Result<(), String>> {
        crate::tool_dispatch::resolve_pre_dispatch(self, tool_calls).await
    }

    /// Execute a batch of tool calls in parallel using a JoinSet.
    async fn execute_tool_calls(
        &self,
        tool_calls: &[ToolCall],
        turn: u32,
        parent_budget_remaining_usd: Option<f64>,
        tool_errors: &mut Vec<hermix_core::ToolErrorRecord>,
    ) -> Vec<ToolResult> {
        // Resolve pre-dispatch decisions for every tool call up front.
        // Allow/Deny resolve synchronously; AwaitApproval blocks on the
        // oneshot channel until upstream resolves the approval request.
        let pre_dispatch = self.resolve_pre_dispatch(tool_calls).await;

        let mut join_set = JoinSet::new();
        let max_delegate_depth = self.resolve_max_delegate_depth();
        let current_delegate_depth = self.delegate_depth;
        let orchestrator = self.sub_agent_orchestrator.clone();

        // Run orchestrated `delegate_task` calls sequentially in the caller's
        // task — this keeps the inner AgentLoop future out of the Send-bound
        // JoinSet and preserves the requested concurrency cap which is already
        // applied upstream via `cap_delegates`.
        let mut orchestrated: Vec<ToolResult> = Vec::new();
        if let Some(orch) = orchestrator.as_ref() {
            for tc in tool_calls {
                if tc.function.name != "delegate_task" {
                    continue;
                }
                // Honor the pre-dispatch hook before the orchestrator spends work on it.
                if let Some(Err(reason)) = pre_dispatch.get(&tc.id) {
                    orchestrated.push(ToolResult::err(&tc.id, reason.clone()));
                    continue;
                }
                // M1.6.2: push a focus-stack frame BEFORE delegating.
                // We pass an empty sub_agent_id — `note_delegation_end`
                // matches the topmost Delegation frame when the id is
                // empty (see its docstring). This decouples push from
                // the orchestrator's internal UUID generation.
                let parsed_for_focus = serde_json::from_str::<Value>(&tc.function.arguments).ok();
                let task_for_focus = parsed_for_focus
                    .as_ref()
                    .and_then(|v| v.get("task"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.note_delegation_start(&task_for_focus, "");
                if current_delegate_depth >= max_delegate_depth {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        format!(
                            "Delegation depth limit reached ({}/{}).",
                            current_delegate_depth, max_delegate_depth
                        ),
                    ));
                    continue;
                }
                // Phase 18+: if the caller passed an explicit
                // `toolset` whitelist in the delegate_task
                // params, refuse any name that's in the
                // sub-agent blocklist. This is a *belt-and-
                // suspenders* check on top of the child
                // registry construction below — we want to
                // catch the bad call early and surface a
                // clear error to the parent LLM.
                if let Some(toolset) = parsed_for_focus
                    .as_ref()
                    .and_then(|v| v.get("toolset"))
                    .and_then(|v| v.as_str())
                {
                    let names: Vec<String> = toolset
                        .split(|c: char| c == ',' || c.is_whitespace())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect();
                    if let Some(bad) =
                        crate::sub_agent_safety::contains_blocked_tool(&names)
                    {
                        orchestrated.push(ToolResult::err(
                            &tc.id,
                            crate::sub_agent_safety::block_reason(&bad),
                        ));
                        continue;
                    }
                }
                let parsed: Value = match serde_json::from_str(&tc.function.arguments) {
                    Ok(v) => v,
                    Err(e) => {
                        orchestrated.push(ToolResult::err(
                            &tc.id,
                            format!(
                                "Invalid JSON params for tool 'delegate_task': {}. \
                                 Please retry with valid JSON.",
                                e
                            ),
                        ));
                        continue;
                    }
                };
                let task = parsed
                    .get("task")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if task.trim().is_empty() {
                    orchestrated.push(ToolResult::err(
                        &tc.id,
                        "delegate_task requires non-empty 'task' string.",
                    ));
                    continue;
                }
                let req = crate::sub_agent_orchestrator::SubAgentRequest {
                    task,
                    context: parsed
                        .get("context")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    toolset: parsed
                        .get("toolset")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    model: parsed
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    child_depth: current_delegate_depth + 1,
                    max_depth: max_delegate_depth,
                    parent_budget_remaining_usd,
                };
                // Orchestrator internally runs the child on its own
                // `tokio::spawn` task, which erases the child future and breaks
                // async recursion between parent / child `execute_tool_calls`.
                let output = orch.execute(req).await;
                orchestrated.push(ToolResult::ok(&tc.id, output));
            }
        }

        // P2.4 (2026-06-30): run remaining interactive tools (currently
        // just `clarify`) sequentially in the caller's task BEFORE
        // launching the JoinSet. This blocks the parallel work until the
        // user has answered the clarifying question — otherwise we'd
        // race the user response and accumulate stale tool results in
        // the LLM context. delegate_task is already handled above by
        // the orchestrator path and is filtered out of this loop.
        for tc in tool_calls {
            if !crate::agent_helpers::is_interactive_tool(&tc.function.name)
                || tc.function.name == "delegate_task"
            {
                continue;
            }
            // Honor pre-dispatch decisions (Allow/Deny/AwaitApproval).
            if let Some(Err(reason)) = pre_dispatch.get(&tc.id) {
                orchestrated.push(ToolResult::err(&tc.id, reason.clone()));
                continue;
            }
            let tool_call_id = tc.id.clone();
            let tool_name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let registry = self.tool_registry.clone();
            let result = match registry.get(&tool_name) {
                Some(entry) => {
                    let params: Value = match serde_json::from_str(&raw_args) {
                        Ok(v) => v,
                        Err(e) => {
                            let err = ToolResult::err(
                                &tool_call_id,
                                format!(
                                    "Invalid JSON params for tool '{}': {}. \
                                     Please check your parameters and retry with valid JSON.",
                                    tool_name, e
                                ),
                            );
                            orchestrated.push(err);
                            continue;
                        }
                    };
                    let handler_result = (entry.handler)(params);
                    match handler_result {
                        Ok(output) => ToolResult::ok(&tool_call_id, output),
                        Err(e) => ToolResult::err(&tool_call_id, e.to_string()),
                    }
                }
                None => {
                    let available = registry.names().join(", ");
                    ToolResult::err(
                        &tool_call_id,
                        format!(
                            "Unknown tool '{}'. Available tools: [{}]",
                            tool_name, available
                        ),
                    )
                }
            };
            orchestrated.push(result);
        }

        for tc in tool_calls {
            // Skip `delegate_task` when an orchestrator already handled it.
            if orchestrator.is_some() && tc.function.name == "delegate_task" {
                continue;
            }
            // P2.4 (2026-06-30): interactive tools run sequentially in
            // the orchestrator pass above (delegate_task) or — for clarify
            // — never enter this JoinSet at all. We extract and handle
            // them right after the orchestrator loop. Skipping them here
            // keeps the JoinSet reserved for genuinely-independent work.
            if crate::agent_helpers::is_interactive_tool(&tc.function.name)
                && !matches!(tc.function.name.as_str(), "delegate_task")
            {
                continue;
            }
            // M1.6.2: push a focus-stack frame for non-orchestrated
            // delegate_task BEFORE spawning the JoinSet future.
            // The pop side (`notify_delegations` → `note_delegation_end`)
            // reads the real sub_agent_id from the tool result content
            // and matches by depth — empty push id is intentional and
            // pairs with the topmost-match policy in `note_delegation_end`.
            if !orchestrator.is_some() && tc.function.name == "delegate_task" {
                let parsed_for_focus = serde_json::from_str::<Value>(&tc.function.arguments).ok();
                let task_for_focus = parsed_for_focus
                    .as_ref()
                    .and_then(|v| v.get("task"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                self.note_delegation_start(&task_for_focus, "");
            }
            let tool_call_id = tc.id.clone();
            let tool_name = tc.function.name.clone();
            let raw_args = tc.function.arguments.clone();
            let registry = self.tool_registry.clone();
            // Clone the per-tool-call decision so the spawned future owns it.
            let pre_decision = pre_dispatch.get(&tool_call_id).cloned();

            let per_call_timeout = std::time::Duration::from_secs(60);
            join_set.spawn(async move {
                // Honor the pre-dispatch hook: skip handler invocation entirely
                // when the user (or upstream policy) denied this tool call.
                if let Some(Err(reason)) = pre_decision.as_ref() {
                    return ToolResult::err(&tool_call_id, reason.clone());
                }
                // A5: per-call timeout around the handler. If a tool
                // hangs (e.g. a `terminal` command waiting for stdin
                // that never arrives, a `web_fetch` on a wedged
                // server), this returns an error envelope instead of
                // wedging the whole batch. 60 s matches the LLM-call
                // timeout in `call_llm_with_retry`.
                let handler_fut = async {
                    match registry.get(&tool_name) {
                        Some(entry) => {
                            // Parse arguments
                            let mut params: Value = match serde_json::from_str(&raw_args) {
                                Ok(v) => v,
                                Err(e) => {
                                    let error_msg = format!(
                                        "Invalid JSON params for tool '{}': {}. \
                                         Please check your parameters and retry with valid JSON.",
                                        tool_name, e
                                    );
                                    return ToolResult::err(&tool_call_id, error_msg);
                                }
                            };

                            if tool_name == "delegate_task" {
                                if current_delegate_depth >= max_delegate_depth {
                                    return ToolResult::err(
                                        &tool_call_id,
                                        format!(
                                            "Delegation depth limit reached ({}/{}).",
                                            current_delegate_depth, max_delegate_depth
                                        ),
                                    );
                                }
                                if let Some(obj) = params.as_object_mut() {
                                    obj.insert(
                                        "child_depth".to_string(),
                                        Value::from(current_delegate_depth + 1),
                                    );
                                    obj.insert(
                                        "max_depth".to_string(),
                                        Value::from(max_delegate_depth),
                                    );
                                    if let Some(remaining) = parent_budget_remaining_usd {
                                        obj.insert(
                                            "parent_budget_remaining_usd".to_string(),
                                            Value::from(remaining),
                                        );
                                    }
                                }
                            }

                            // Execute the handler
                            match (entry.handler)(params) {
                                Ok(output) => ToolResult::ok(&tool_call_id, output),
                                Err(e) => ToolResult::err(&tool_call_id, e.to_string()),
                            }
                        }
                        None => {
                            let available = registry.names().join(", ");
                            let error_msg = format!(
                                "Unknown tool '{}'. Available tools: [{}]",
                                tool_name, available
                            );
                            ToolResult::err(&tool_call_id, error_msg)
                        }
                    }
                };
                match tokio::time::timeout(per_call_timeout, handler_fut).await {
                    Ok(result) => result,
                    Err(_elapsed) => ToolResult::err(
                        &tool_call_id,
                        format!(
                            "per-call timeout after {}s: tool '{}' did not return in time",
                            per_call_timeout.as_secs(),
                            tool_name,
                        ),
                    ),
                }
            });
        }

        // Collect results in INPUT order. tool_calls.iter().enumerate()
        // gives us a stable position; orchestrated (delegate_task,
        // interactive tools) and the JoinSet (everything else) both
        // place their result into the same fixed-size Vec<Option<…>>.
        // See A5 commit for the order-preservation rationale: the
        // caller (run / run_stream) feeds results back to the LLM in
        // the same order as the tool_calls it produced, so a delayed
        // JoinSet result landing in slot 0 before a fast one landing
        // in slot 2 would scramble the LLM's view of which result
        // corresponds to which tool call.
        let total = tool_calls.len();
        let mut ordered: Vec<Option<ToolResult>> = (0..total).map(|_| None).collect();
        let index_by_id: std::collections::HashMap<&str, usize> = tool_calls
            .iter()
            .enumerate()
            .map(|(i, tc)| (tc.id.as_str(), i))
            .collect();

        for tool_result in orchestrated {
            if tool_result.is_error {
                let tc = tool_calls
                    .iter()
                    .find(|tc| tc.id == tool_result.tool_call_id);
                if let Some(tc) = tc {
                    tool_errors.push(hermix_core::ToolErrorRecord {
                        tool_name: tc.function.name.clone(),
                        error: tool_result.content.clone(),
                        turn,
                    });
                }
            }
            if let Some(&idx) = index_by_id.get(tool_result.tool_call_id.as_str()) {
                ordered[idx] = Some(tool_result);
            } else {
                // An orchestrated result without a matching input id
                // would be a programming error — surface it rather
                // than silently drop the result.
                tracing::error!(
                    tool_call_id = %tool_result.tool_call_id,
                    "orchestrated result had no matching tool_call; pushing in completion order as fallback"
                );
                // Fallback: push into the first empty slot.
                if let Some(slot) = ordered.iter_mut().find(|s| s.is_none()) {
                    *slot = Some(tool_result);
                }
            }
        }

        // A5: total batch timeout. Per-call timeout is set per-spawn
        // above (60 s); the total here caps the whole batch at 5 min
        // so a pathological case (every tool near-completion-but-
        // never-returns) still terminates.
        let total_timeout = std::time::Duration::from_secs(300);
        let deadline = std::time::Instant::now() + total_timeout;

        while ordered.iter().any(Option::is_none) {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                // Batch-wide timeout: fill every still-pending slot
                // with a synthetic error envelope.
                for (idx, slot) in ordered.iter_mut().enumerate() {
                    if slot.is_none() {
                        let tc = &tool_calls[idx];
                        let err = ToolResult::err(
                            &tc.id,
                            format!(
                                "batch-wide timeout after {}s: tool '{}' did not return in time",
                                total_timeout.as_secs(),
                                tc.function.name,
                            ),
                        );
                        tool_errors.push(hermix_core::ToolErrorRecord {
                            tool_name: tc.function.name.clone(),
                            error: err.content.clone(),
                            turn,
                        });
                        *slot = Some(err);
                    }
                }
                break;
            }
            match tokio::time::timeout(remaining, join_set.join_next()).await {
                Ok(Some(Ok(tool_result))) => {
                    if tool_result.is_error {
                        let tc = tool_calls
                            .iter()
                            .find(|tc| tc.id == tool_result.tool_call_id);
                        if let Some(tc) = tc {
                            tool_errors.push(hermix_core::ToolErrorRecord {
                                tool_name: tc.function.name.clone(),
                                error: tool_result.content.clone(),
                                turn,
                            });
                        }
                    }
                    if let Some(&idx) = index_by_id.get(tool_result.tool_call_id.as_str()) {
                        ordered[idx] = Some(tool_result);
                    } else {
                        tracing::error!(
                            tool_call_id = %tool_result.tool_call_id,
                            "JoinSet result had no matching tool_call; pushing as fallback"
                        );
                        if let Some(slot) = ordered.iter_mut().find(|s| s.is_none()) {
                            *slot = Some(tool_result);
                        }
                    }
                }
                Ok(Some(Err(join_err))) => {
                    // We don't track the task_id → tool_call_id
                    // mapping here (the JoinSet path doesn't need it
                    // for the normal case), so we can't pinpoint
                    // which call panicked. Place the error in the
                    // first empty slot and log the dead task id.
                    // If no empty slot exists (everything else
                    // already returned), the outer loop will exit on
                    // its own and the missing_slot fallback will
                    // catch the gap.
                    let dead_id = join_err.id();
                    if let Some((idx, slot)) = ordered
                        .iter_mut()
                        .enumerate()
                        .find(|(_, s)| s.is_none())
                    {
                        let tc = &tool_calls[idx];
                        tool_errors.push(hermix_core::ToolErrorRecord {
                            tool_name: tc.function.name.clone(),
                            error: format!("Task join error: {join_err}"),
                            turn,
                        });
                        *slot = Some(ToolResult::err(
                            &tc.id,
                            format!("Task join error (id={dead_id}): {join_err}"),
                        ));
                    } else {
                        tracing::error!(
                            "execute_tool_calls: JoinSet join error with no empty slot: {join_err}"
                        );
                    }
                }
                Ok(None) => break, // JoinSet drained before we filled every slot
                Err(_elapsed) => continue, // outer loop will catch zero remaining
            }
        }

        ordered
            .into_iter()
            .map(|slot| {
                slot.unwrap_or_else(|| {
                    // Final safety net: every slot should be filled by
                    // the loops above, but if not (e.g. an inner panic
                    // and a rerouted fallback), synthesise an error
                    // rather than panic the agent.
                    ToolResult::err(
                        "missing",
                        "execute_tool_calls: no result produced for this slot (internal error)".to_string(),
                    )
                })
            })
            .collect()
    }

    fn resolve_max_delegate_depth(&self) -> u32 {
        crate::tool_dispatch::resolve_max_delegate_depth()
    }

    /// Cap concurrent delegate_task calls based on config.
    fn cap_delegates(&self, tool_calls: &mut Vec<ToolCall>) {
        crate::tool_dispatch::cap_delegates(self, tool_calls)
    }

    fn emit_background_review_metrics(&self, turn: u32, ctx: &ContextManager) {
        if !self.config.background_review_metrics_enabled {
            return;
        }
        let snapshot = ctx.get_messages().to_vec();
        tokio::spawn(async move {
            let tool_msg_count = snapshot
                .iter()
                .filter(|m| matches!(m.role, hermix_core::MessageRole::Tool))
                .count();
            tracing::debug!(
                turn,
                tool_messages = tool_msg_count,
                total_messages = snapshot.len(),
                "Background review snapshot captured"
            );
        });
    }

    /// Phase 2.4: terminal-persistence shared by `run()` and `run_stream()`
    /// natural-finish paths. Extracted to keep the two call sites in
    /// lock-step (P3 added `maybe_freeze_snapshot` to run_stream and we
    /// don't want that fix to drift again).
    fn persist_session_terminal(
        &self,
        ctx: &ContextManager,
        session_id: &str,
        total_turns: u32,
        review_memory_at_end: bool,
    ) {
        let (u, a) = extract_last_user_assistant(ctx.get_messages());
        self.memory_sync(&u, &a, session_id);
        // Phase 18: freeze a final snapshot at session end so the next
        // session can pick up without replaying every frame.
        self.maybe_freeze_snapshot(session_id, total_turns);
        self.spawn_background_review(total_turns, ctx, review_memory_at_end);
    }

    /// Metrics (always) + optional Python-style memory/skill review LLM pass on session end.
    fn spawn_background_review(&self, turn: u32, ctx: &ContextManager, review_memory_at_end: bool) {
        self.emit_background_review_metrics(turn, ctx);
        if !self.config.background_review_enabled {
            return;
        }
        let mut review_skills = false;
        // P0-1: only fire the skill review when the user hasn't
        // opted out via `auto_skill_extraction: false` in
        // ~/.hermix/config.yaml. Without this guard, the master
        // off-switch would be ignored by the background loop.
        if self.config.auto_skill_extraction
            && self.config.skill_creation_nudge_interval > 0
            && self
                .tool_registry
                .names()
                .iter()
                .any(|n| n == "skill_manage")
        {
            if let Ok(mut c) = self.evolution_counters.lock() {
                if c.iters_since_skill >= self.config.skill_creation_nudge_interval {
                    review_skills = true;
                    c.iters_since_skill = 0;
                }
            }
        }
        let review_memory = review_memory_at_end;
        if !review_memory && !review_skills {
            return;
        }
        let prompt: &'static str = match (review_memory, review_skills) {
            (true, true) => COMBINED_REVIEW_PROMPT,
            (true, false) => MEMORY_REVIEW_PROMPT,
            (false, true) => SKILL_REVIEW_PROMPT,
            _ => return,
        };
        let mut hist = ctx.get_messages().to_vec();
        hist.push(Message::user(prompt));
        let mut cfg = self.config.clone();
        cfg.background_review_enabled = false;
        cfg.background_review_metrics_enabled = false;
        cfg.memory_nudge_interval = 0;
        cfg.skill_creation_nudge_interval = 0;
        // P0-1: child session must NOT write skills back to the
        // user's primary skill library — the background-review
        // task is itself a child of the parent's agent loop, and
        // a misbehaving reflection would pollute
        // `~/.hermix/skills/`. Skill creation only happens at the
        // top-level agent.
        cfg.auto_skill_extraction = false;
        cfg.max_concurrent_delegates = 0;
        cfg.quiet_mode = true;
        cfg.max_turns = cfg.max_turns.min(8);
        let tools = self.tool_registry.clone();
        let provider = self.llm_provider.clone();
        let review_cb = self.callbacks.background_review_callback.clone();
        tokio::spawn(async move {
            let agent = AgentLoop::new(cfg, tools, provider);
            match agent.run(hist, None).await {
                Ok(result) => {
                    if let Some(cb) = review_cb.as_ref() {
                        if let Some(summary) = summarize_background_review_result(&result.messages)
                        {
                            cb(&summary);
                        }
                    }
                }
                Err(e) => {
                    tracing::debug!(error = %e, "background memory/skill review failed");
                }
            }
        });
    }

    /// Recover todo-state hints from historical messages at loop start.
    fn hydrate_todo_store(&self, ctx: &ContextManager) {
        let todo_markers = ctx
            .get_messages()
            .iter()
            .filter_map(|m| m.content.as_deref())
            .filter(|c| c.contains("TODO") || c.contains("[ ]") || c.contains("[x]"))
            .count();
        if todo_markers > 0 {
            tracing::debug!(todo_markers, "Hydrated todo markers from prior context");
        }
    }

    /// See [`PostLlmAction`]. Mutates `inner_thinking` / `inner_empty`
    /// when bumping the per-attempt retry budget and emits a `lifecycle`
    /// status line on each transition.
    fn classify_post_llm_response(
        &self,
        r: hermix_core::LlmResponse,
        inner_thinking: &mut u32,
        inner_empty: &mut u32,
        ctx: &mut ContextManager,
    ) -> PostLlmAction {
        let has_tools = r
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|tc| !tc.is_empty());
        if has_tools {
            return PostLlmAction::Break(r);
        }
        if Self::assistant_visible_text(&r.message) {
            return PostLlmAction::Break(r);
        }
        if Self::assistant_has_reasoning(&r.message)
            && *inner_thinking < self.config.thinking_prefill_max_retries
        {
            *inner_thinking += 1;
            self.emit_status(
                "lifecycle",
                &format!(
                    "Reasoning-only response — retrying ({}/{})",
                    *inner_thinking, self.config.thinking_prefill_max_retries
                ),
            );
            ctx.add_message(r.message.clone());
            return PostLlmAction::RetryReasoning;
        }
        if !Self::assistant_has_reasoning(&r.message)
            && *inner_empty < self.config.empty_content_max_retries
        {
            *inner_empty += 1;
            tracing::warn!(
                "empty assistant response — retrying ({}/{})",
                *inner_empty,
                self.config.empty_content_max_retries
            );
            self.emit_status(
                "lifecycle",
                &format!(
                    "Empty assistant response — retrying ({}/{})",
                    *inner_empty, self.config.empty_content_max_retries
                ),
            );
            return PostLlmAction::RetryEmpty;
        }
        PostLlmAction::Break(r)
    }
}

/// File-change summary emitted after a successful mutating tool call.
/// Used by the chat UI to render a WorkBuddy-style "文件变更 (N个)" row
/// inline in the conversation flow.
#[derive(Debug, Clone)]
struct FileChangeInfo {
    path: String,
    change_type: &'static str, // "created" | "modified" | "deleted" | "renamed"
    summary: Option<String>,
}

/// Phase 4.1 (2026-07-03): the per-attempt classification that sits
/// inside the `let response = loop { ... }` retry block in both
/// `run()` and `run_stream()`. Identical ~40 lines in both call
/// sites — has_tools / visible_text / reasoning-retry / empty-retry /
/// exhausted. Extracted here so the two retry loops can keep their
/// own transport (sync vs stream-first) and call sites can `match`
/// on the resulting action.
///
/// The helper takes `r` by value because the caller is going to
/// hand ownership of the response to either a downstream consumer
/// (`Break(r)`) or the context (the reasoning-retry branch already
/// pushed `r.message` onto `ctx` before returning `RetryReasoning`,
/// so `r` itself is dropped on the caller side).
///
/// `pub(crate)` so the `mod tests` block below can `match` on the
/// variants; an impl-block-local enum would be private to the
/// `AgentLoop` impl and unreachable from tests.
#[derive(Debug)]
pub(crate) enum PostLlmAction {
    /// LLM produced tool calls or visible text — leave the retry
    /// loop with this response.
    Break(hermix_core::LlmResponse),
    /// Reasoning-only response; the message has been pushed onto
    /// `ctx` and the caller should re-enter the retry loop.
    RetryReasoning,
    /// Empty response (no reasoning, no content, no tools); the
    /// caller should re-enter the retry loop without touching
    /// `ctx`.
    RetryEmpty,
}

/// Look at a completed tool call and decide whether it produced a
/// filesystem change worth surfacing in the chat summary. We only
/// handle the three mutating tools (write_file / patch / search_files),
/// and we only emit on success.
fn extract_file_change(
    tool_name: &str,
    arguments: &str,
    output: &str,
) -> Option<FileChangeInfo> {
    let args: serde_json::Value = serde_json::from_str(arguments).ok()?;
    let path = match tool_name {
        "write_file" | "patch" => args.get("path")?.as_str()?.to_string(),
        "search_files" => return None, // read-only
        _ => return None,
    };
    if path.is_empty() {
        return None;
    }
    // Heuristic: write_file to a new path = "created", overwrite = "modified".
    // patch is always "modified". We can't always tell from the tool
    // output alone, so we default to "modified" for write_file (the
    // common case in our agent flow) and the operator can refine later.
    let change_type = match tool_name {
        "write_file" => "modified",
        "patch" => "modified",
        _ => "modified",
    };
    // Pull a brief summary from the tool output's first non-empty line
    // (most write_file handlers return "<N bytes written>" or similar).
    let summary = output
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.chars().take(120).collect::<String>());
    Some(FileChangeInfo {
        path,
        change_type,
        summary,
    })
}

/// Extract the last user and assistant content from a message slice for memory sync.
fn extract_last_user_assistant(messages: &[Message]) -> (String, String) {
    let user = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermix_core::MessageRole::User))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    let assistant = messages
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermix_core::MessageRole::Assistant))
        .and_then(|m| m.content.clone())
        .unwrap_or_default();
    (user, assistant)
}

fn summarize_background_review_result(messages: &[Message]) -> Option<String> {
    let mut actions: Vec<String> = Vec::new();
    for msg in messages {
        if !matches!(msg.role, hermix_core::MessageRole::Tool) {
            continue;
        }
        let Some(raw) = msg.content.as_deref() else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        if !data
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let message = data
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let target = data
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if message.is_empty() {
            continue;
        }
        let lower = message.to_ascii_lowercase();
        if lower.contains("created") || lower.contains("updated") {
            actions.push(message);
        } else if lower.contains("added") || (!target.is_empty() && lower.contains("add")) {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        } else if message.contains("Entry added") {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        } else if lower.contains("removed") || lower.contains("replaced") {
            let label = match target.as_str() {
                "memory" => "Memory",
                "user" => "User profile",
                _ => target.as_str(),
            };
            if !label.is_empty() {
                actions.push(format!("{label} updated"));
            }
        }
    }
    if actions.is_empty() {
        return None;
    }
    let mut deduped: Vec<String> = Vec::new();
    for action in actions {
        if !deduped.iter().any(|a| a == &action) {
            deduped.push(action);
        }
    }
    Some(format!("💾 {}", deduped.join(" · ")))
}

// Physically moved to `cost_guard.rs` (2026-07-04) alongside the
// `CostGuard` struct. Re-exported here so `merge_usage` call sites
// (`run()` / `run_stream()` turn-usage accumulation) keep compiling
// without renaming inline usages.
pub(crate) use crate::cost_guard::merge_usage;

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::JsonSchema;
    use crate::cost_guard::{WarnEffect, estimate_usage_cost_usd};

    #[test]
    fn p1_loop_state_defaults_to_zero() {
        // Phase 1 (2026-07-03): `LoopState::default` must produce a
        // zero-initialized struct that is `#[derive(Default)]` so the
        // refactor of `run()` / `run_stream()` can keep using `..Default::default()`.
        //
        // Phase 5 (2026-07-04): `total_api_time_ms` / `total_tool_time_ms`
        // moved into `state.timings`; access path now `s.timings.*`.
        // `last_checkpoint_messages` / `last_content_with_tools` moved
        // into `state.message_cache`; access path now
        // `s.message_cache.checkpoint().is_none()` /
        // `s.message_cache.last_content_with_tools.is_none()`.
        let s = LoopState::default();
        assert_eq!(s.total_turns, 0);
        assert_eq!(s.timings.total_api_time_ms, 0);
        assert_eq!(s.timings.total_tool_time_ms, 0);
        assert!(s.cost_guard.accumulated_usage.is_none());
        assert_eq!(s.cost_guard.session_cost_usd, 0.0);
        assert!(!s.cost_guard.cost_warned);
        assert!(s.cost_guard.forced_runtime_route.is_none());
        assert!(s.message_cache.checkpoint().is_none());
        assert_eq!(s.retry_counters.invalid_tool, 0);
        assert_eq!(s.retry_counters.invalid_json, 0);
        assert_eq!(s.retry_counters.truncated_tool, 0);
        assert!(s.message_cache.last_content_with_tools.is_none());
        assert_eq!(s.context_pressure.warned_tier, 0.0);
        assert!(s.context_pressure.last_warn_at.is_none());
        assert_eq!(s.context_pressure.last_warn_percent, 0.0);
    }

    #[test]
    fn p1_loop_state_counters_increment_independently() {
        // Sanity: the per-turn counters can be updated without touching
        // unrelated state — required because `run()` and `run_stream()`
        // will both mutate these fields on every turn.
        //
        // Phase 5 (2026-07-04): timings now use `state.timings.record_*`
        // (saturating-add) instead of a top-level `+= 1`. We exercise
        // the new path here and assert it doesn't bleed into other
        // counters.
        let mut s = LoopState::default();
        s.total_turns += 1;
        s.retry_counters.bump_invalid_tool();
        s.retry_counters.bump_invalid_json();
        s.retry_counters.bump_invalid_json();
        s.cost_guard.session_cost_usd += 0.001;
        s.timings.record_api(42);
        s.timings.record_tool(99);
        assert_eq!(s.total_turns, 1);
        assert_eq!(s.retry_counters.invalid_tool, 1);
        assert_eq!(s.retry_counters.invalid_json, 2);
        assert!(s.cost_guard.session_cost_usd > 0.0);
        // Unrelated fields stay zero.
        assert_eq!(s.timings.total_api_time_ms, 42);
        assert_eq!(s.timings.total_tool_time_ms, 99);
        assert!(s.cost_guard.accumulated_usage.is_none());
    }

    #[test]
    fn p5_turn_timings_record_independently() {
        // Phase 5 (2026-07-04): `record_api` and `record_tool` must
        // accumulate independently (each on its own `u64`), saturating
        // on overflow (a long session shouldn't wrap around). Default
        // must be all zeros so `LoopState::default()` semantics stay
        // unchanged from the pre-extraction two-`u64`-field version.
        let mut t = TurnTimings::default();
        assert_eq!(t.total_api_time_ms, 0);
        assert_eq!(t.total_tool_time_ms, 0);

        t.record_api(100);
        t.record_api(50);
        t.record_tool(200);
        assert_eq!(t.total_api_time_ms, 150);
        assert_eq!(t.total_tool_time_ms, 200);

        // Saturating: u64::MAX + 1 stays at u64::MAX instead of wrapping
        // to 0 (which would silently under-report a long session).
        let mut t2 = TurnTimings::default();
        t2.record_api(u64::MAX);
        t2.record_api(1);
        assert_eq!(t2.total_api_time_ms, u64::MAX);
    }

    #[test]
    fn p5_message_cache_store_and_take_independently() {
        // Phase 5 (2026-07-04): `store_checkpoint` /
        // `store_content_with_tools` / `take_content_with_tools` /
        // `checkpoint` / `content_with_tools` must all behave
        // independently (the two caches are unrelated fields, not
        // aliases of each other) and `take_content_with_tools` must
        // clear the slot behind it (so a second `take` returns
        // `None`). Default must be all-`None` so
        // `LoopState::default()` semantics stay unchanged.
        let mut c = MessageCache::default();
        assert!(c.checkpoint().is_none());
        assert!(c.last_content_with_tools.is_none());

        let snapshot = vec![Message::user("checkpoint text")];
        c.store_checkpoint(snapshot.clone());
        c.store_content_with_tools(Some("visible text".to_string()));
        assert_eq!(c.checkpoint().unwrap(), &snapshot);
        assert_eq!(
            c.last_content_with_tools.as_deref().unwrap(),
            "visible text"
        );

        // take_content_with_tools moves the value out and clears the
        // slot; checkpoint is untouched (take is per-slot).
        let taken = c.take_content_with_tools();
        assert_eq!(taken, Some("visible text".to_string()));
        assert!(c.last_content_with_tools.is_none());
        assert!(c.checkpoint().is_some());

        // store_content_with_tools(None) is a no-op for callers that
        // want to clear without consuming (in case `None` means
        // "explicitly nothing" vs. the implicit `None` after a take).
        c.store_content_with_tools(None);
        assert!(c.last_content_with_tools.is_none());
    }

    #[test]
    fn p1_1_loop_state_round_trip() {
        // Phase 1.1 (2026-07-03): the bridge between `run()`'s 14
        // loop-local aliases and the 14 `state` fields. Publishing the
        // locals back into `state` is what makes the future
        // `run_shared_loop(state, llm_caller)` extraction possible
        // (the shared body only sees `&mut LoopState`).
        //
        // Phase 5 (2026-07-03): the `publish_from` bridge shrank from
        // 9 args to 5 args. The 4 cost-guard args (accumulated_usage /
        // session_cost_usd / cost_warned / forced_runtime_route) are
        // owned by `state.cost_guard` and are no longer bridged. The 3
        // retry-counter args and the 3 context-pressure args were
        // already removed in earlier Phase 5 sub-commits.
        //
        // Phase 5 (2026-07-04, 1st follow-up): shrank from 5 args to
        // 3 args. The 2 timing args (`total_api_time_ms` /
        // `total_tool_time_ms`) are owned by `state.timings` and
        // mutated in place by `record_api` / `record_tool`.
        //
        // Phase 5 (2026-07-04, 2nd follow-up): shrank from 3 args to
        // 1 arg. The 2 message-cache args
        // (`last_checkpoint_messages` / `last_content_with_tools`) are
        // owned by `state.message_cache` and mutated in place by
        // `store_checkpoint` / `store_content_with_tools`. We
        // exercise all 4 in-place paths in this test.
        //
        // Phase 5 (2026-07-04, 3rd follow-up): the `publish_from`
        // helper is removed entirely. The single remaining arg
        // (`total_turns`) is written directly to `state.total_turns`
        // in both `run()` and `run_stream()`. The bridge is dead.
        let mut s = LoopState::default();
        // total_turns — the only field still bridged, now assigned
        // directly without going through `publish_from`.
        s.total_turns = 7;
        assert_eq!(s.total_turns, 7);
        // timings live on the inner struct — exercise the in-place
        // record path that replaced the old `+= api_elapsed` locals.
        s.timings.record_api(1234);
        s.timings.record_tool(567);
        assert_eq!(s.timings.total_api_time_ms, 1234);
        assert_eq!(s.timings.total_tool_time_ms, 567);
        // message_cache lives on the inner struct — exercise the
        // store / take / checkpoint / content_with_tools paths that
        // replaced the old `last_*` local aliases.
        let snapshot = vec![Message::user("checkpoint text")];
        s.message_cache.store_checkpoint(snapshot.clone());
        s.message_cache.store_content_with_tools(Some("visible text".to_string()));
        assert_eq!(s.message_cache.checkpoint().unwrap(), &snapshot);
        assert_eq!(
            s.message_cache.last_content_with_tools.as_deref().unwrap(),
            "visible text"
        );
        let taken = s.message_cache.take_content_with_tools();
        assert_eq!(taken, Some("visible text".to_string()));
        assert!(s.message_cache.last_content_with_tools.is_none());
        // cost_guard defaults are unchanged by the bridge — owned by
        // the inner struct and mutated in place by the runtime.
        assert!(s.cost_guard.accumulated_usage.is_none());
        assert_eq!(s.cost_guard.session_cost_usd, 0.0);
        assert!(!s.cost_guard.cost_warned);
        assert!(s.cost_guard.forced_runtime_route.is_none());
        // retry_counters live on the inner struct — set directly here
        // to mirror pre-extraction semantics (the inner struct is
        // mutated in place by bump_* / reset_* methods).
        s.retry_counters.invalid_tool = 2;
        s.retry_counters.invalid_json = 3;
        s.retry_counters.truncated_tool = 1;
        assert_eq!(s.retry_counters.invalid_tool, 2);
        assert_eq!(s.retry_counters.invalid_json, 3);
        assert_eq!(s.retry_counters.truncated_tool, 1);
        // context_pressure is on the inner struct — set directly here to
        // mirror pre-extraction semantics (the inner struct is mutated
        // in place by should_emit_warning).
        s.context_pressure.warned_tier = 0.65;
        s.context_pressure.last_warn_percent = 0.71;
        assert_eq!(s.context_pressure.warned_tier, 0.65);
        assert_eq!(s.context_pressure.last_warn_percent, 0.71);
    }

    #[test]
    fn test_agent_config_default() {
        let config = AgentConfig::default();
        assert_eq!(config.max_turns, 30);
        assert_eq!(config.model, "gpt-4o");
        assert!(!config.stream);
        assert_eq!(config.max_concurrent_delegates, 1);
        assert_eq!(config.memory_flush_interval, 5);
        assert_eq!(config.api_mode, ApiMode::ChatCompletions);
        assert_eq!(config.retry.max_retries, 3);
        assert!(config.session_id.is_none());
        assert!(!config.skip_memory);
        assert!(config.platform.is_none());
        assert!(!config.pass_session_id);
        assert!(config.max_cost_usd.is_none());
        assert_eq!(config.cost_guard_degrade_at_ratio, 0.8);
        assert!(config.cost_guard_degrade_model.is_none());
        assert_eq!(config.checkpoint_interval_turns, 3);
        assert_eq!(config.rollback_on_tool_error_threshold, 3);
        assert!(!config.smart_model_routing.enabled);
        assert!(config.background_review_metrics_enabled);
    }

    // ── M1.6.5: build_session_summary regression tests ──
    //
    // These exercise the helper that produces the session-level
    // FocusConclusion written to memory.db at session end. Without
    // this, single-shot sessions (no delegate_task) leave memory.db
    // empty and cross-session recall finds nothing.

    fn msg_user(s: &str) -> Message {
        Message::user(s)
    }
    fn msg_assistant(s: &str) -> Message {
        Message::assistant(s)
    }

    #[test]
    fn build_session_summary_returns_none_without_user_turns() {
        let msgs = vec![msg_assistant("hi")];
        assert!(AgentLoop::build_session_summary(&msgs).is_none());
    }

    #[test]
    fn build_session_summary_returns_none_for_empty_user_content() {
        let msgs = vec![msg_user("   ")];
        assert!(AgentLoop::build_session_summary(&msgs).is_none());
    }

    #[test]
    fn build_session_summary_picks_last_user_and_assistant() {
        let msgs = vec![
            msg_user("first question"),
            msg_assistant("first answer"),
            msg_user("second question"),
            msg_assistant("second answer"),
        ];
        let s = AgentLoop::build_session_summary(&msgs).expect("some summary");
        assert!(s.contains("second question"));
        assert!(s.contains("second answer"));
        assert!(!s.contains("first question"));
    }

    #[test]
    fn build_session_summary_handles_user_only() {
        let msgs = vec![msg_user("hello")];
        let s = AgentLoop::build_session_summary(&msgs).expect("some summary");
        assert_eq!(s, "USER: hello");
    }

    #[test]
    fn build_session_summary_truncates_long_user_and_assistant() {
        let long_user: String = "x".repeat(500);
        let long_assistant: String = "y".repeat(1000);
        let msgs = vec![msg_user(&long_user), msg_assistant(&long_assistant)];
        let s = AgentLoop::build_session_summary(&msgs).expect("some summary");
        // The 200-char user truncation produces 200 chars + "…" appended
        // to the prefix — assert no runaway growth.
        assert!(
            s.chars().count() < 700,
            "summary should be truncated, got {} chars",
            s.chars().count()
        );
        assert!(s.contains("…"), "long content should be truncated with …");
    }

    #[test]
    fn test_plugin_hooks_include_api_and_session_finalize_paths() {
        use crate::plugins::{Plugin, PluginContext, PluginMeta};
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        struct CaptureHooksPlugin {
            seen: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl Plugin for CaptureHooksPlugin {
            fn meta(&self) -> PluginMeta {
                PluginMeta {
                    name: "capture-hooks".to_string(),
                    version: "0.1.0".to_string(),
                    description: "capture hook events".to_string(),
                    author: None,
                }
            }

            async fn initialize(&self) -> Result<(), AgentError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                Ok(())
            }

            fn register(&self, ctx: &mut PluginContext) {
                let hook_types = [
                    HookType::PreApiRequest,
                    HookType::PostApiRequest,
                    HookType::OnSessionEnd,
                    HookType::OnSessionFinalize,
                    HookType::OnSessionReset,
                ];
                for hook in hook_types {
                    let seen = self.seen.clone();
                    ctx.on(
                        hook,
                        Arc::new(move |_ctx| {
                            seen.lock()
                                .expect("seen lock")
                                .push(hook.as_str().to_string());
                            HookResult::Ok
                        }),
                    );
                }
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut pm = PluginManager::new();
        pm.register(Arc::new(CaptureHooksPlugin { seen: seen.clone() }));
        let pm = Arc::new(std::sync::Mutex::new(pm));

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_plugins(pm);

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("/reset")], None))
            .expect("agent run should succeed");

        let seen = seen.lock().expect("seen lock");
        for required in [
            "pre_api_request",
            "post_api_request",
            "on_session_end",
            "on_session_finalize",
            "on_session_reset",
        ] {
            assert!(
                seen.iter().any(|name| name == required),
                "missing required hook: {required}; seen={:?}",
                *seen
            );
        }
    }

    #[test]
    fn test_plugin_hooks_include_api_paths_in_stream_mode() {
        use crate::plugins::{Plugin, PluginContext, PluginMeta};
        use futures::stream::BoxStream;

        struct StreamingProvider;
        #[async_trait::async_trait]
        impl LlmProvider for StreamingProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::iter(vec![
                    Ok(StreamChunk {
                        delta: Some(hermix_core::StreamDelta {
                            content: Some("stream-done".to_string()),
                            tool_calls: None,
                            extra: None,
                        }),
                        finish_reason: None,
                        usage: None,
                    }),
                    Ok(StreamChunk {
                        delta: None,
                        finish_reason: Some("stop".to_string()),
                        usage: None,
                    }),
                ])
                .boxed()
            }
        }

        struct CaptureHooksPlugin {
            seen: Arc<std::sync::Mutex<Vec<String>>>,
        }
        #[async_trait::async_trait]
        impl Plugin for CaptureHooksPlugin {
            fn meta(&self) -> PluginMeta {
                PluginMeta {
                    name: "capture-hooks-stream".to_string(),
                    version: "0.1.0".to_string(),
                    description: "capture stream hook events".to_string(),
                    author: None,
                }
            }

            async fn initialize(&self) -> Result<(), AgentError> {
                Ok(())
            }

            async fn shutdown(&self) -> Result<(), AgentError> {
                Ok(())
            }

            fn register(&self, ctx: &mut PluginContext) {
                for hook in [HookType::PreApiRequest, HookType::PostApiRequest] {
                    let seen = self.seen.clone();
                    ctx.on(
                        hook,
                        Arc::new(move |_ctx| {
                            seen.lock()
                                .expect("seen lock")
                                .push(hook.as_str().to_string());
                            HookResult::Ok
                        }),
                    );
                }
            }
        }

        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut pm = PluginManager::new();
        pm.register(Arc::new(CaptureHooksPlugin { seen: seen.clone() }));
        let pm = Arc::new(std::sync::Mutex::new(pm));

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(StreamingProvider),
        )
        .with_plugins(pm);

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run_stream(
                vec![Message::user("stream hello")],
                None,
                Some(Box::new(|_| {})),
            ))
            .expect("stream run should succeed");

        let seen = seen.lock().expect("seen lock");
        assert!(seen.iter().any(|name| name == "pre_api_request"));
        assert!(seen.iter().any(|name| name == "post_api_request"));
    }

    #[test]
    fn test_llm_request_model_strips_provider_prefix() {
        use futures::stream::BoxStream;

        struct CaptureModelProvider {
            seen_model: Arc<std::sync::Mutex<Option<String>>>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for CaptureModelProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                *self.seen_model.lock().expect("seen_model lock") = model.map(|m| m.to_string());
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let seen_model = Arc::new(std::sync::Mutex::new(None));
        let provider = CaptureModelProvider {
            seen_model: seen_model.clone(),
        };
        let config = AgentConfig {
            model: "openrouter:deepseek/deepseek-v3.2".to_string(),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(ToolRegistry::new()), Arc::new(provider));

        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let seen = seen_model.lock().expect("seen_model lock");
        assert_eq!(seen.as_deref(), Some("deepseek/deepseek-v3.2"));
    }

    /// 2026-06-29: end-to-end check that
    /// `auto_compress_if_over_threshold` emits at least one
    /// `compaction_status` snapshot (pre- and post-attempt) and that
    /// the snapshot carries the live pressure numbers. Mirrors the
    /// `status_callback_receives_context_pressure_messages` test
    /// above — that one covered the *string* `lifecycle` event, this
    /// one covers the *structured* `BreakerSnapshot` event the
    /// `CompactionStatusBanner` consumes.
    #[tokio::test]
    async fn compaction_status_callback_receives_snapshots() {
        use crate::compression::{CompressorConfig, ContextCompressor};
        use futures::stream::BoxStream;
        use hermix_intelligence::auxiliary::{
            AuxiliaryClient, AuxiliarySource, ProviderCandidate,
        };
        use std::sync::Arc;

        struct LocalDummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for LocalDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        // Capture every snapshot the callback sees — the test
        // asserts both the pressure-detected pre-snapshot and the
        // post-attempt snapshot arrive with non-zero pressure.
        let captured: Arc<std::sync::Mutex<Vec<crate::compression::BreakerSnapshot>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            on_compaction_status: Some(Arc::new(move |snap| {
                cap_ref
                    .lock()
                    .expect("compaction capture lock")
                    .push(snap.clone());
            })),
            ..Default::default()
        };

        // Build a minimal compressor with an in-process auxiliary
        // provider so `compress()` can produce a real summary.
        // Pattern mirrored from `compression::tests::aux_with_provider`.
        let aux = Arc::new(
            AuxiliaryClient::builder()
                .add_candidate(ProviderCandidate::new(
                    AuxiliarySource::Custom,
                    "test-model",
                    Arc::new(LocalDummyProvider) as Arc<dyn LlmProvider>,
                ))
                .build(),
        );
        let mut cfg = CompressorConfig::default();
        cfg.context_length = 1000;
        cfg.threshold_percent = 0.8;
        cfg.quiet_mode = true;
        let compressor = ContextCompressor::new(cfg, aux);

        // Use a separate dummy provider for the *agent* LLM (the
        // compressor's aux client has its own provider chain — they
        // don't share, hence the second instance).
        let aux_provider: Arc<dyn LlmProvider> =
            Arc::new(LocalDummyProvider);
        // HACK: `LocalDummyProvider` isn't Clone, so we re-construct
        // a fresh struct to satisfy the second `Arc::new` call. The
        // state the LLM holds is empty, so identity is irrelevant.
        struct AuxProviderShim;
        #[async_trait::async_trait]
        impl LlmProvider for AuxProviderShim {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let _ = aux_provider; // suppress unused warning — the agent LLM is a shim.

        let mut agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(AuxProviderShim),
        )
        .with_callbacks(callbacks);
        agent.compression = Some(Arc::new(tokio::sync::Mutex::new(compressor)));

        // Force the context over threshold. `ContextManager::new(100)`
        // sets max=100; we zero the output reserve so the pressure
        // threshold is the documented 80 tokens. The user message
        // needs to tokenise to > 80 — `estimate_tokens` divides by
        // 4, so 400 chars yields 100 tokens, comfortably above.
        //
        // `compress()` requires `n_messages > protect_first_n + 4`
        // (7 messages by default), so we add several user+assistant
        // pairs to give the compressor real middle-turns to summarise.
        let mut ctx = ContextManager::new(100).with_output_reserve(0);
        for i in 0..12 {
            ctx.add_message(Message::user(format!("user {} {}", i, "x".repeat(80))));
            ctx.add_message(Message::assistant(format!("assistant {} {}", i, "y".repeat(80))));
        }
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        let snaps = captured.lock().expect("snaps lock");
        assert!(
            snaps.len() >= 2,
            "expected pressure + post-attempt snapshots, got {}",
            snaps.len()
        );
        // Pre-snapshot must carry the live pressure so the banner
        // can render "x / 80 tokens" before any LLM work.
        assert!(snaps[0].used_tokens > 0, "pre-snap used_tokens=0");
        assert_eq!(snaps[0].threshold_tokens, 80, "pre-snap threshold");
        // Post-snapshot should reflect a successful compression —
        // state stays Closed because no failures have been recorded.
        let last = snaps.last().expect("at least one snap");
        assert_eq!(last.state, crate::compression::BreakerState::Closed);
        assert!(last.compression_count >= 1);
    }

    #[test]
    fn summarize_background_review_nothing_to_save() {
        let msgs = vec![Message::assistant("Nothing to save.")];
        let out = summarize_background_review_result(&msgs);
        assert!(out.is_none());
    }

    #[test]
    fn summarize_background_review_counts_tool_calls() {
        let msgs = vec![
            Message::tool_result(
                "tc_mem",
                "{\"success\":true,\"message\":\"Skill 'prospect-scanner' created.\"}",
                Some("memory"),
            ),
            Message::tool_result(
                "tc_skill",
                "{\"success\":true,\"message\":\"Entry added\",\"target\":\"memory\"}",
                Some("skill"),
            ),
            Message::tool_result("tc_skip", "{\"success\":false,\"message\":\"failed\"}", Some("skip")),
        ];
        let out = summarize_background_review_result(&msgs).expect("summary should exist");
        assert!(out.starts_with("💾 "));
        assert!(out.contains("Skill 'prospect-scanner' created."));
        assert!(out.contains("Memory updated"));
    }

    #[tokio::test]
    async fn status_callback_receives_context_pressure_messages() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        let rows = captured.lock().expect("captured lock");
        assert!(rows
            .iter()
            .any(|(kind, msg)| kind == "lifecycle" && msg.contains("triggering compression")));
    }

    #[tokio::test]
    async fn status_callback_receives_empty_response_retry_notice() {
        use futures::stream::BoxStream;

        #[derive(Default)]
        struct RetryDummyProvider {
            calls: std::sync::Mutex<u32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RetryDummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                let mut n = self.calls.lock().expect("calls lock");
                *n += 1;
                let msg = if *n == 1 {
                    Message::assistant("")
                } else {
                    Message::assistant("ok")
                };
                Ok(hermix_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            max_turns: 1,
            empty_content_max_retries: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(RetryDummyProvider::default()),
        )
        .with_callbacks(callbacks);

        let result = agent.run(vec![Message::user("hello")], None).await;
        assert!(result.is_ok());
        let rows = captured.lock().expect("captured lock");
        assert!(rows.iter().any(|(kind, msg)| {
            kind == "lifecycle" && msg.contains("Empty assistant response — retrying")
        }));
    }

    #[tokio::test]
    async fn stream_collect_emits_activity_while_waiting_for_chunks() {
        use async_stream::stream;
        use futures::stream::BoxStream;

        struct DelayedStreamProvider;

        #[async_trait::async_trait]
        impl LlmProvider for DelayedStreamProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("noop"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                Box::pin(stream! {
                    tokio::time::sleep(Duration::from_millis(3500)).await;
                    yield Ok(StreamChunk {
                        delta: Some(hermix_core::StreamDelta {
                            content: Some("hi".into()),
                            tool_calls: None,
                            extra: None,
                        }),
                        finish_reason: None,
                        usage: None,
                    });
                    yield Ok(StreamChunk {
                        delta: None,
                        finish_reason: Some("stop".into()),
                        usage: None,
                    });
                })
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            stream_activity_interval_secs: 1,
            stream_activity_stall_secs: 1,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DelayedStreamProvider),
        )
        .with_callbacks(callbacks);

        let mut ctx = ContextManager::new(10_000);
        ctx.add_message(Message::user("ping"));

        let noop: &(dyn Fn(StreamChunk) + Send + Sync) = &|_chunk| {};
        let outcome = agent
            .collect_stream_llm_response(&ctx, &[], None, "openai:gpt-4o", noop)
            .await
            .expect("stream collect");

        assert!(matches!(outcome, StreamCollectOutcome::Complete(_)));
        let rows = captured.lock().expect("captured lock");
        let warm = rows
            .iter()
            .any(|(k, m)| k == "activity" && m.contains("Still waiting"));
        let stall = rows
            .iter()
            .any(|(k, m)| k == "activity" && m.contains("may be stuck"));
        assert!(
            warm && stall,
            "expected one warm + one stall activity; got {:?}",
            *rows
        );
    }

    #[tokio::test]
    async fn quiet_mode_suppresses_status_callback() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let captured = Arc::new(std::sync::Mutex::new(Vec::<(String, String)>::new()));
        let cap_ref = captured.clone();
        let callbacks = AgentCallbacks {
            status_callback: Some(Arc::new(move |kind, msg| {
                cap_ref
                    .lock()
                    .expect("status callback lock")
                    .push((kind.to_string(), msg.to_string()));
            })),
            ..Default::default()
        };

        let cfg = AgentConfig {
            quiet_mode: true,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(cfg, Arc::new(ToolRegistry::new()), Arc::new(DummyProvider))
            .with_callbacks(callbacks);
        let mut ctx = ContextManager::new(100);
        ctx.add_message(Message::user("x".repeat(90)));
        agent.auto_compress_if_over_threshold(&mut ctx).await;

        assert!(captured.lock().expect("captured lock").is_empty());
    }

    #[test]
    fn test_builtin_personality_injected_into_system_prompt() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            personality: Some("coder".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(prompt.contains("## Active Personality (coder)"));
        assert!(prompt.contains("`coder` persona"));
    }

    #[test]
    fn test_unknown_personality_name_does_not_add_overlay_block() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            personality: Some("unknown_persona".to_string()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(!prompt.contains("## Active Personality (unknown_persona)"));
    }

    #[test]
    fn test_formatting_discipline_guidance_in_system_prompt() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        // No personality, no tools, no plugin context — formatting discipline
        // and thinking discipline must still be present in every prompt.
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(
            prompt.contains("Output formatting discipline"),
            "formatting discipline block missing from system prompt"
        );
        assert!(prompt.contains("DO NOT"));
        assert!(prompt.contains("Code identifiers"));
        assert!(
            prompt.contains("Output format — strict"),
            "output format contract block missing from system prompt"
        );
        assert!(prompt.contains("Hard rules"));
        assert!(prompt.contains("Correct example"));
        assert!(prompt.contains("Wrong example"));
    }

    #[test]
    fn test_active_workspace_block_injected_when_set() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        // No workspace → no block.
        let no_ws_agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = no_ws_agent.build_system_prompt("", &[], "gpt-4o");
        assert!(
            !prompt.contains("## Active Workspace"),
            "workspace block should be absent when no workspace is configured"
        );

        // With workspace → block surfaces the path verbatim.
        let mut config = AgentConfig::default();
        config.workspace = Some("/work/qcc".to_string());
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let prompt = agent.build_system_prompt("", &[], "gpt-4o");
        assert!(
            prompt.contains("## Active Workspace"),
            "workspace block missing"
        );
        assert!(
            prompt.contains("/work/qcc"),
            "workspace path missing from block"
        );
    }

    #[test]
    fn test_tool_usage_guidance_in_system_prompt() {
        // TOOL_USAGE_GUIDANCE must be present in every prompt (any model, any toolset)
        // because the "use a generic tool when a purpose-built one exists" failure
        // mode is universal. Don't gate it on tool_enforcement model list.
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig::default();
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );

        // 1. Guidance present for any model (even one not in the tool-enforcement list).
        let prompt = agent.build_system_prompt("", &[], "unknown-model-xyz");
        assert!(
            prompt.contains("Tool selection discipline"),
            "TOOL_USAGE_GUIDANCE block must be unconditional, not gated on model"
        );
        assert!(
            prompt.contains("scan the available tool list"),
            "decision pattern step 1 (scan tools) missing"
        );
        assert!(
            prompt.contains("call it first"),
            "decision pattern step 2 (use purpose-built tool first) missing"
        );
        assert!(
            prompt.contains("Anti-patterns"),
            "anti-pattern block missing"
        );

        // 2. MiniMax models must now be in the tool-enforcement whitelist so they
        // get the "must use tools" guidance in addition to the selection discipline.
        assert!(
            agent.should_inject_tool_enforcement("minimax-m2.7"),
            "MiniMax M2.7 should be in tool-enforcement whitelist"
        );
        assert!(
            agent.should_inject_tool_enforcement("M2-highspeed"),
            "M2 variant should be in tool-enforcement whitelist"
        );
        // Spot-check existing whitelist still works.
        assert!(agent.should_inject_tool_enforcement("gpt-4o"));
        assert!(!agent.should_inject_tool_enforcement("claude-sonnet-4"));
    }

    #[test]
    fn test_smart_model_routing_cheap_route_for_simple_turn() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("帮我总结一下今天要做什么")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-4o-mini")
        );
    }

    #[test]
    fn test_runtime_provider_command_args_override_primary_acp_metadata() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: Some("copilot-language-server".to_string()),
                args: vec![
                    "--stdio".to_string(),
                    "--model".to_string(),
                    "gpt-4o-mini".to_string(),
                ],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            provider: Some("openai".to_string()),
            runtime_providers,
            acp_command: Some("global-acp".to_string()),
            acp_args: vec!["--global".to_string()],
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let primary = crate::cost_routing::primary_runtime_snapshot(&agent);
        assert_eq!(primary.command.as_deref(), Some("copilot-language-server"));
        assert_eq!(
            primary.args,
            vec![
                "--stdio".to_string(),
                "--model".to_string(),
                "gpt-4o-mini".to_string()
            ]
        );
    }

    #[test]
    fn test_smart_model_routing_codex_provider_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "codex".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-test-key".to_string()),
                base_url: Some("https://api.openai.com/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("codex".to_string()),
                    model: Some("gpt-5-mini".to_string()),
                    base_url: Some("https://api.openai.com/v1".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("总结一下这个需求")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert_eq!(
            selected.as_ref().map(|r| r.model.as_str()),
            Some("gpt-5-mini")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("codex")
        );
        assert_eq!(
            selected.as_ref().and_then(|r| r.api_mode.as_ref()),
            Some(&ApiMode::CodexResponses)
        );
    }

    #[test]
    fn test_smart_model_routing_qwen_oauth_alias_builds_runtime() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "qwen-oauth".to_string(),
            RuntimeProviderConfig {
                api_key: Some("sk-qwen-oauth".to_string()),
                base_url: Some("https://dashscope.aliyuncs.com/compatible-mode/v1".to_string()),
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("qwen-oauth".to_string()),
                    model: Some("qwen3-coder-plus".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("给我一段简短总结")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("qwen-oauth")
        );
    }

    #[test]
    fn test_smart_model_routing_openai_codex_reads_auth_store_token() {
        use futures::stream::BoxStream;
        use std::time::{SystemTime, UNIX_EPOCH};

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let home = std::env::temp_dir().join(format!("hermes-oauth-fixture-{}", nonce));
        let auth_dir = home.join("auth");
        std::fs::create_dir_all(&auth_dir).expect("create auth dir");
        std::fs::write(
            auth_dir.join("tokens.json"),
            r#"{
  "openai-codex": {
    "provider": "openai-codex",
    "access_token": "codex-oauth-token",
    "token_type": "bearer",
    "refresh_token": null,
    "scope": null,
    "expires_at": "2099-01-01T00:00:00Z"
  }
}"#,
        )
        .expect("write token store");

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "openai-codex".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: None,
                command: None,
                args: Vec::new(),
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            hermix_home: Some(home.to_string_lossy().to_string()),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai-codex".to_string()),
                    model: Some("gpt-5-codex".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("openai-codex")
        );
        // Best effort cleanup.
        let _ = std::fs::remove_dir_all(home);
    }

    #[test]
    fn test_self_evolution_skill_counter_ticks_each_iteration() {
        use futures::stream::BoxStream;
        use hermix_core::JsonSchema;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            // Opt out: these tests assert on iteration counters, which
            // require the loop to terminate after the model emits a final
            // text-only answer. autonomous_mode (default true) would keep
            // nudging the model back into action.
            autonomous_mode: false,
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    fn test_self_evolution_parity_fixtures_v2026_4_13_memory_nudge() {
        use futures::stream::BoxStream;
        use hermix_core::JsonSchema;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("done"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        // Fixture-style cases distilled from Python v2026.4.16:
        // - counter persists across runs
        // - resets to 0 when hitting interval threshold
        #[derive(Clone, Copy)]
        struct Case {
            runs: u32,
            expected_turns_since_memory: u32,
        }
        let cases = vec![
            Case {
                runs: 1,
                expected_turns_since_memory: 1,
            },
            Case {
                runs: 2,
                expected_turns_since_memory: 0,
            },
        ];

        for case in cases {
            let mut registry = ToolRegistry::new();
            registry.register(
                "memory",
                ToolSchema::new("memory", "Memory tool", JsonSchema::new("object")),
                Arc::new(|_args| Ok("{\"success\":true}".to_string())),
            );

            let config = AgentConfig {
                memory_nudge_interval: 2,
                ..AgentConfig::default()
            };
            let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(DummyProvider));
            let rt = tokio::runtime::Runtime::new().expect("runtime");
            for _ in 0..case.runs {
                let _ = rt
                    .block_on(agent.run(vec![Message::user("hello")], None))
                    .expect("agent run should succeed");
            }
            let counters = agent.evolution_counters.lock().expect("counter lock");
            assert_eq!(
                counters.turns_since_memory, case.expected_turns_since_memory,
                "fixture runs={} mismatch",
                case.runs
            );
        }
    }

    #[test]
    fn test_iters_since_skill_resets_then_reincrements_on_followup_iteration() {
        use futures::stream::BoxStream;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct TwoStepProvider {
            calls: Arc<AtomicU32>,
        }

        #[async_trait::async_trait]
        impl LlmProvider for TwoStepProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let msg = if n == 0 {
                    Message::assistant_with_tool_calls(
                        None,
                        vec![hermix_core::ToolCall {
                            id: "tc_skill".to_string(),
                            function: hermix_core::FunctionCall {
                                name: "skill_manage".to_string(),
                                arguments: "{}".to_string(),
                            },
                        }],
                    )
                } else {
                    Message::assistant("done")
                };
                Ok(hermix_core::LlmResponse {
                    message: msg,
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(
            "skill_manage",
            ToolSchema::new("skill_manage", "Manage skills", JsonSchema::new("object")),
            Arc::new(|_args| Ok("{\"success\":true}".to_string())),
        );

        let config = AgentConfig {
            skill_creation_nudge_interval: 10,
            // Opt out: these tests assert on iteration counters, which
            // require the loop to terminate after the model emits a final
            // text-only answer. autonomous_mode (default true) would keep
            // nudging the model back into action.
            autonomous_mode: false,
            ..AgentConfig::default()
        };
        let provider = TwoStepProvider {
            calls: Arc::new(AtomicU32::new(0)),
        };
        let agent = AgentLoop::new(config, Arc::new(registry), Arc::new(provider));
        let rt = tokio::runtime::Runtime::new().expect("runtime");
        let _ = rt
            .block_on(agent.run(vec![Message::user("hello")], None))
            .expect("agent run should succeed");

        let counters = agent.evolution_counters.lock().expect("counter lock");
        // Iteration #1 increments then skill_manage resets to 0.
        // Iteration #2 (final assistant turn) increments again to 1.
        // Python follows the same cadence because `_iters_since_skill += 1`
        // happens at each loop iteration before the tool/reset branch.
        assert_eq!(counters.iters_since_skill, 1);
    }

    #[test]
    fn test_smart_model_routing_copilot_acp_missing_cli_falls_back() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: Some("acp://copilot".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp://copilot".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert!(
            selected.is_none(),
            "missing ACP CLI should fail cheap-route and fall back"
        );
    }

    #[test]
    fn test_smart_model_routing_copilot_acp_tcp_mode_skips_cli_check() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let mut runtime_providers = HashMap::new();
        runtime_providers.insert(
            "copilot-acp".to_string(),
            RuntimeProviderConfig {
                api_key: None,
                base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                command: Some("definitely-not-installed-copilot-cli".to_string()),
                args: vec!["--acp".to_string(), "--stdio".to_string()],
                oauth_token_url: None,
                oauth_client_id: None,
            },
        );

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            runtime_providers,
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("copilot-acp".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: Some("acp+tcp://127.0.0.1:8765".to_string()),
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let selected = agent.resolve_smart_runtime_route(&[Message::user("帮我总结这段话")]);
        assert_eq!(
            selected.as_ref().and_then(|r| r.provider.as_deref()),
            Some("copilot-acp")
        );
    }

    #[test]
    fn test_smart_model_routing_skips_complex_turn() {
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }

            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            model: "openai:gpt-4o".to_string(),
            smart_model_routing: SmartModelRoutingConfig {
                enabled: true,
                max_simple_chars: 160,
                max_simple_words: 28,
                cheap_model: Some(CheapModelRouteConfig {
                    provider: Some("openai".to_string()),
                    model: Some("gpt-4o-mini".to_string()),
                    base_url: None,
                    api_key_env: None,
                }),
            },
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let messages = vec![Message::user("请帮我 debug 这段 traceback 并修复错误")];
        let selected = agent.resolve_smart_runtime_route(&messages);
        assert!(selected.is_none());
    }

    #[test]
    fn test_deduplicate_tool_calls() {
        let calls = vec![
            ToolCall {
                id: "1".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "2".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.txt"}"#.into(),
                },
            },
            ToolCall {
                id: "3".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: r#"{"path":"b.txt"}"#.into(),
                },
            },
        ];
        let deduped = AgentLoop::deduplicate_tool_calls(&calls);
        assert_eq!(deduped.len(), 2);
        assert_eq!(deduped[0].id, "1");
        assert_eq!(deduped[1].id, "3");
    }

    #[test]
    fn test_memory_write_event_from_tool_call_add() {
        let tc = ToolCall {
            id: "c1".into(),
            function: hermix_core::FunctionCall {
                name: "memory".into(),
                arguments:
                    r#"{"action":"add","target":"user","content":"Prefers concise answers"}"#.into(),
            },
        };
        let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "add");
        assert_eq!(event.1, "user");
        assert_eq!(event.2, "Prefers concise answers");
    }

    #[test]
    fn test_memory_write_event_from_tool_call_remove_uses_old_text() {
        let tc = ToolCall {
            id: "c2".into(),
            function: hermix_core::FunctionCall {
                name: "memory".into(),
                arguments: r#"{"action":"remove","target":"memory","old_text":"obsolete fact"}"#
                    .into(),
            },
        };
        let event = AgentLoop::memory_write_event_from_tool_call(&tc).unwrap();
        assert_eq!(event.0, "remove");
        assert_eq!(event.1, "memory");
        assert_eq!(event.2, "obsolete fact");
    }

    #[test]
    fn test_hydrate_session_search_args_injects_current_session_id() {
        use futures::stream::BoxStream;
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<hermix_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-auto-1".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s1".into(),
            function: hermix_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"previous issue","limit":3}"#.into(),
            },
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-auto-1")
        );
    }

    #[test]
    fn test_hydrate_session_search_args_keeps_existing_current_session_id() {
        use futures::stream::BoxStream;
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<hermix_core::StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let config = AgentConfig {
            session_id: Some("sess-outer".into()),
            ..AgentConfig::default()
        };
        let agent = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut tc = ToolCall {
            id: "s2".into(),
            function: hermix_core::FunctionCall {
                name: "session_search".into(),
                arguments: r#"{"query":"abc","current_session_id":"sess-explicit"}"#.into(),
            },
        };
        agent.hydrate_session_search_args(&mut tc);
        let args: Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(
            args.get("current_session_id").and_then(|v| v.as_str()),
            Some("sess-explicit")
        );
    }

    #[test]
    fn test_budget_warning() {
        let config = AgentConfig {
            max_turns: 10,
            ..AgentConfig::default()
        };
        let registry = Arc::new(ToolRegistry::new());
        use futures::stream::BoxStream;

        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                Ok(hermix_core::LlmResponse {
                    message: Message::assistant("dummy"),
                    usage: None,
                    model: "dummy".into(),
                    finish_reason: Some("stop".into()),
                })
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }

        let agent = AgentLoop::new(config, registry, Arc::new(DummyProvider));

        let max = agent.config.max_turns;
        assert!(budget_pressure_text(6, max, 0.7, 0.9, true).is_none());
        assert!(budget_pressure_text(7, max, 0.7, 0.9, true).is_some());
        let w = budget_pressure_text(9, max, 0.7, 0.9, true).unwrap();
        assert!(w.contains("BUDGET WARNING"), "{w}");
        let w10 = budget_pressure_text(10, max, 0.7, 0.9, true).unwrap();
        assert!(w10.contains("BUDGET WARNING"), "{w10}");
        let _ = agent;
    }

    #[test]
    fn test_tool_registry_new() {
        let registry = ToolRegistry::new();
        assert!(registry.names().is_empty());
    }

    #[test]
    fn test_merge_usage() {
        let a = UsageStats {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            estimated_cost: Some(0.01),
        };
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: Some(0.02),
        };
        let merged = merge_usage(Some(a), &b);
        assert_eq!(merged.prompt_tokens, 300);
        assert_eq!(merged.completion_tokens, 150);
        assert_eq!(merged.total_tokens, 450);
        assert_eq!(merged.estimated_cost, Some(0.03));
    }

    #[test]
    fn test_merge_usage_none() {
        let b = UsageStats {
            prompt_tokens: 200,
            completion_tokens: 100,
            total_tokens: 300,
            estimated_cost: None,
        };
        let merged = merge_usage(None, &b);
        assert_eq!(merged.prompt_tokens, 200);
    }

    #[test]
    fn test_estimate_usage_cost_prefers_reported_estimate() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1000,
            completion_tokens: 1000,
            total_tokens: 2000,
            estimated_cost: Some(0.42),
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o", &cfg).unwrap();
        assert!((cost - 0.42).abs() < 1e-9);
    }

    #[test]
    fn test_estimate_usage_cost_uses_model_fallback_table() {
        let cfg = AgentConfig::default();
        let u = UsageStats {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
            total_tokens: 2_000_000,
            estimated_cost: None,
        };
        let cost = estimate_usage_cost_usd(&u, "openai:gpt-4o-mini", &cfg).unwrap();
        assert!((cost - 0.75).abs() < 1e-9);
    }

    // ── ContextPressureMonitor unit tests (2026-07-03) ──
    //
    // The helper is the gate that decides whether `run()` / `run_stream()`
    // should fire a `lifecycle` warning when the context window fills up.
    // It had no direct unit tests — coverage only came through the
    // integration path. These tests exercise the three decision rules
    // (tier upgrade, cooldown, percent step) using deterministic state
    // and rely on the helper's own `Instant::now()` for time, so no
    // wall-clock waiting is needed.

    #[test]
    fn cp_monitor_default_is_all_zero_and_none() {
        // Phase 5: the freshly-extracted ContextPressureMonitor must
        // behave like the old 3 inline fields when not yet primed —
        // otherwise the first call inside run() / run_stream() would
        // see stale defaults.
        let m = ContextPressureMonitor::default();
        assert_eq!(m.warned_tier, 0.0);
        assert!(m.last_warn_at.is_none());
        assert_eq!(m.last_warn_percent, 0.0);
    }

    #[test]
    fn cp_monitor_returns_false_when_tier_zero() {
        let mut m = ContextPressureMonitor::default();
        let emit = m.should_emit_warning(0.42, 0.0);
        assert!(!emit);
        // No state mutation when no emission.
        assert_eq!(m.warned_tier, 0.0);
        assert!(m.last_warn_at.is_none());
        assert_eq!(m.last_warn_percent, 0.0);
    }

    #[test]
    fn cp_monitor_emits_on_first_call_above_zero() {
        let mut m = ContextPressureMonitor::default();
        let emit = m.should_emit_warning(0.65, 0.65);
        assert!(emit);
        // Tier upgrade always emits and records tier + warn_at + percent.
        assert_eq!(m.warned_tier, 0.65);
        assert!(m.last_warn_at.is_some());
        assert_eq!(m.last_warn_percent, 65.0);
    }

    #[test]
    fn cp_monitor_blocks_within_cooldown_without_tier_upgrade() {
        let mut m = ContextPressureMonitor::default();
        // Seed: same tier, recent emit at 65%.
        m.warned_tier = 0.65;
        m.last_warn_at = Some(Instant::now());
        m.last_warn_percent = 65.0;
        // Same tier, <5% advance, immediately after prior emit → cooldown blocks.
        let emit = m.should_emit_warning(0.66, 0.65);
        assert!(!emit);
        // State unchanged.
        assert_eq!(m.warned_tier, 0.65);
        assert_eq!(m.last_warn_percent, 65.0);
    }

    #[test]
    fn cp_monitor_emits_on_tier_upgrade_even_within_cooldown() {
        let mut m = ContextPressureMonitor::default();
        // Seed: tier=0.65, just warned at 65%.
        m.warned_tier = 0.65;
        m.last_warn_at = Some(Instant::now());
        m.last_warn_percent = 65.0;
        // Tier upgrade overrides the cooldown.
        let emit = m.should_emit_warning(0.66, 0.85);
        assert!(emit);
        assert_eq!(m.warned_tier, 0.85);
        assert!(m.last_warn_at.is_some());
    }

    // ── Phase 5 (2026-07-03): RetryCounters unit tests ──
    //
    // The 3 sibling retry counters on LoopState (invalid_tool_retries,
    // invalid_json_retries, truncated_tool_call_retries) collapsed into
    // a single RetryCounters struct. These tests pin the bump + reset
    // contract; the `< max` / `>= max` decision stays at the call site
    // because the action on max differs between the three.

    #[test]
    fn retry_counters_default_is_all_zero() {
        // Phase 5: the freshly-constructed RetryCounters must start at
        // zero for all three slots — otherwise a stale value would leak
        // into the first call inside run() / run_stream().
        let c = RetryCounters::default();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 0);
        assert_eq!(c.truncated_tool, 0);
    }

    #[test]
    fn retry_counters_bump_returns_saturating_incremented_value() {
        // All three bump methods return the new value and saturate
        // instead of overflowing — required because a buggy provider
        // could otherwise pin a counter at u32::MAX forever.
        let mut c = RetryCounters::default();
        assert_eq!(c.bump_invalid_tool(), 1);
        assert_eq!(c.bump_invalid_tool(), 2);
        assert_eq!(c.bump_invalid_json(), 1);
        assert_eq!(c.bump_invalid_json(), 2);
        assert_eq!(c.bump_truncated_tool(), 1);
        // Saturating behavior: simulate pathological case.
        c.invalid_tool = u32::MAX;
        assert_eq!(c.bump_invalid_tool(), u32::MAX);
        // Other counters untouched by the saturating bump.
        assert_eq!(c.invalid_json, 2);
        assert_eq!(c.truncated_tool, 1);
    }

    #[test]
    fn retry_counters_reset_clears_only_the_targeted_slot() {
        // The three reset methods are independent — resetting one
        // counter must not touch the other two, otherwise a successful
        // turn (which resets invalid_tool + invalid_json) could
        // accidentally wipe the streaming-only truncated_tool counter.
        let mut c = RetryCounters::default();
        c.bump_invalid_tool();
        c.bump_invalid_tool();
        c.bump_invalid_json();
        c.bump_truncated_tool();
        c.reset_invalid_tool();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 1);
        assert_eq!(c.truncated_tool, 1);
        c.reset_invalid_json();
        assert_eq!(c.invalid_tool, 0);
        assert_eq!(c.invalid_json, 0);
        assert_eq!(c.truncated_tool, 1);
        c.reset_truncated_tool();
        assert_eq!(c.truncated_tool, 0);
    }

    #[test]
    fn retry_counters_truncated_tool_bump_at_max_still_increments() {
        // The truncated_tool call site checks `< max` BEFORE bumping
        // (run_stream's "Truncated tool arguments — retrying" branch
        // only fires when under the cap). After the bump-method
        // refactor, the bump happens unconditionally and the call
        // site does the `<= max` check after. The end-state observable
        // behavior is the same: at the cap, the bump-then-check path
        // is a no-op for the emit, and the next reset() wipes the
        // counter. This test pins the bumped value so a future
        // refactor that introduces an early-return would be caught.
        let mut c = RetryCounters::default();
        c.truncated_tool = 2; // simulate "just under the cap of 3"
        assert_eq!(c.bump_truncated_tool(), 3); // bumps to 3
        assert_eq!(c.bump_truncated_tool(), 4); // would be 4 (over the cap)
        c.reset_truncated_tool();
        assert_eq!(c.truncated_tool, 0);
    }

    // ── Phase 5 (2026-07-03): CostGuard struct + evaluate_thresholds ──
    //
    // The 4 cost-guard fields that used to be 4 sibling `&mut` args on
    // `process_post_response` now live on a single `CostGuard` struct.
    // `evaluate_thresholds` returns an enum (NoAction / Downgrade /
    // Warn / Trip) so the caller — not the helper — owns the side
    // effects (route build + system message).

    #[test]
    fn cost_guard_default_is_all_zeros_and_none() {
        let cg = CostGuard::default();
        assert!(cg.accumulated_usage.is_none());
        assert_eq!(cg.session_cost_usd, 0.0);
        assert!(!cg.cost_warned);
        assert!(cg.forced_runtime_route.is_none());
    }

    #[test]
    fn cost_guard_record_usage_merges_into_accumulated_and_adds_cost() {
        let mut cg = CostGuard::default();
        // First usage: 10 prompt + 20 completion → starts a new total.
        cg.record_usage(
            Some(&UsageStats {
                prompt_tokens: 10,
                completion_tokens: 20,
                total_tokens: 30,
                estimated_cost: Some(0.001),
            }),
            "test-model",
            &AgentConfig::default(),
        );
        assert!(cg.accumulated_usage.is_some());
        assert!(cg.session_cost_usd >= 0.0);
    }

    #[test]
    fn cost_guard_record_usage_no_op_when_turn_usage_is_none() {
        let mut cg = CostGuard::default();
        cg.record_usage(None, "test-model", &AgentConfig::default());
        assert!(cg.accumulated_usage.is_none());
        assert_eq!(cg.session_cost_usd, 0.0);
    }

    #[test]
    fn cost_guard_evaluate_returns_no_action_when_no_limit() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 999.0;
        let action = cg.evaluate_thresholds(None, 0.5, || None);
        assert_eq!(action, CostGuardAction { warn: None, trip: None });
        assert!(!cg.cost_warned);
    }

    #[test]
    fn cost_guard_evaluate_returns_downgrade_when_warn_threshold_first_crossed() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06; // above 0.1 * 0.5
        let action = cg.evaluate_thresholds(Some(0.1), 0.5, || Some("openai:gpt-4o-mini".into()));
        match action.warn {
            Some(WarnEffect { kind: WarnKind::Downgrade { model }, .. }) => {
                assert_eq!(model, "openai:gpt-4o-mini");
            }
            other => panic!("expected Downgrade warn effect, got {:?}", other),
        }
        assert!(action.trip.is_none());
        assert!(cg.cost_warned);
        // forced_runtime_route is NOT set by evaluate_thresholds —
        // the caller must call `install_downgrade` with the built
        // `TurnRuntimeRoute`. This is the whole reason the helper
        // doesn't construct the route itself.
        assert!(cg.forced_runtime_route.is_none());
    }

    #[test]
    fn cost_guard_evaluate_returns_warn_when_degrade_model_unresolvable() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06;
        let action = cg.evaluate_thresholds(Some(0.1), 0.5, || None);
        assert!(matches!(
            action.warn,
            Some(WarnEffect { kind: WarnKind::Warn, .. })
        ));
        assert!(action.trip.is_none());
        assert!(cg.cost_warned);
    }

    #[test]
    fn cost_guard_evaluate_is_idempotent_after_first_warn() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.06;
        // First crossing fires the downgrade side effect.
        let first = cg.evaluate_thresholds(Some(0.1), 0.5, || Some("cheap".into()));
        assert!(matches!(
            first.warn,
            Some(WarnEffect { kind: WarnKind::Downgrade { .. }, .. })
        ));
        // Caller installs the route (mirrors what process_post_response
        // does in production). We only need *some* route to verify the
        // forced_runtime_route field gets populated; a partial struct
        // literal is fine.
        cg.install_downgrade(TurnRuntimeRoute {
            model: "cheap".into(),
            provider: None,
            base_url: None,
            api_key_env: None,
            api_mode: None,
            command: None,
            args: Vec::new(),
            credential_pool: None,
            credential_pool_fallback: true,
            route_label: None,
            routing_reason: Some("cost_guard".into()),
            signature: TurnRouteSignature {
                model: "cheap".into(),
                provider: None,
                base_url: None,
                api_mode: ApiMode::ChatCompletions,
                command: None,
                args: Vec::new(),
            },
        });
        assert!(cg.forced_runtime_route.is_some());
        // Second evaluation: already past warn, no degrade, not yet
        // over the absolute limit → no warn + no trip.
        let second = cg.evaluate_thresholds(Some(0.1), 0.5, || Some("another".into()));
        assert_eq!(second, CostGuardAction { warn: None, trip: None });
    }

    #[test]
    fn cost_guard_evaluate_returns_trip_when_spend_exceeds_limit() {
        let mut cg = CostGuard::default();
        cg.session_cost_usd = 0.5;
        let action = cg.evaluate_thresholds(Some(0.1), 0.5, || Some("cheap".into()));
        let trip = action
            .trip
            .expect("expected trip effect, got None");
        assert!((trip.session_cost_usd - 0.5).abs() < 1e-9);
        assert!((trip.limit - 0.1).abs() < 1e-9);
        // The trip path also flipped cost_warned on the way through
        // (since 0.5 > 0.1 * 0.5 = 0.05). Both warn AND trip fired —
        // caller fires both side effects in order.
        assert!(cg.cost_warned);
        assert!(matches!(
            action.warn,
            Some(WarnEffect { kind: WarnKind::Downgrade { .. }, .. })
        ));
    }

    // ── Phase 1.3a (2026-07-03): process_post_response unit tests ──
    //
    // The shared post-LLM helper extracted in 1.3a replaces ~200 lines
    // of duplication between `run()` and `run_stream()`. These tests
    // exercise the two return paths (Finalize / Continue) and the
    // usage-accumulation side effect.

    #[test]
    fn p1_3a_process_post_response_finalize_on_no_tool_calls() {
        // No tool calls in response → Finalize. The helper should also
        // copy the assistant visible text into `last_content_with_tools`
        // for ack-continuation tracking.
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                unimplemented!()
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let response = hermix_core::LlmResponse {
            message: Message::assistant("final answer"),
            usage: None,
            model: "test".into(),
            finish_reason: Some("stop".into()),
        };
        let mut cost_guard = CostGuard::default();
        // Phase 5 (2026-07-04): `&mut Option<String>` arg became
        // `&mut MessageCache`; the old `last_content` local is gone,
        // we exercise the same final-answer path through
        // `message_cache.last_content_with_tools`.
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0, // total_turns
                0, // api_elapsed
                &mut cost_guard,
                &mut message_cache,
                false, // stream_mode
            )
            .unwrap();
        assert!(matches!(action, PostResponseAction::Finalize));
        assert_eq!(message_cache.last_content_with_tools.as_deref(), Some("final answer"));
    }

    #[test]
    fn p1_3a_process_post_response_cost_guard_trips_when_limit_exceeded() {
        // `session_cost_usd` already over `max_cost_usd` → CostGuardTripped
        // (caller should return finalize with finished_naturally=false).
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                unimplemented!()
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let mut cfg = AgentConfig::default();
        cfg.max_cost_usd = Some(0.01);
        let agent = AgentLoop::new(
            cfg,
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let response = hermix_core::LlmResponse {
            message: Message::assistant("anything"),
            usage: None,
            model: "test".into(),
            finish_reason: Some("stop".into()),
        };
        let mut cost_guard = CostGuard::default();
        cost_guard.session_cost_usd = 0.5; // already 50x over the 0.01 limit
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0,
                0,
                &mut cost_guard,
                &mut message_cache,
                false,
            )
            .unwrap();
        assert_eq!(action, PostResponseAction::CostGuardTripped);
    }

    #[test]
    fn p1_3a_process_post_response_continue_on_tool_calls() {
        // Tool calls in response → Continue. The helper should leave
        // `last_content_with_tools` untouched (caller handles that).
        struct DummyProvider;
        #[async_trait::async_trait]
        impl LlmProvider for DummyProvider {
            async fn chat_completion(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> Result<hermix_core::LlmResponse, AgentError> {
                unimplemented!()
            }
            fn chat_completion_stream(
                &self,
                _messages: &[Message],
                _tools: &[ToolSchema],
                _max_tokens: Option<u32>,
                _temperature: Option<f64>,
                _model: Option<&str>,
                _extra_body: Option<&serde_json::Value>,
            ) -> futures::stream::BoxStream<'static, Result<StreamChunk, AgentError>> {
                futures::stream::empty().boxed()
            }
        }
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(DummyProvider),
        );
        let mut ctx = ContextManager::for_model(&agent.config.model);
        let mut msg = Message::assistant("calling tool");
        msg.tool_calls = Some(vec![ToolCall {
            id: "tc-1".into(),
            function: hermix_core::FunctionCall {
                name: "echo".into(),
                arguments: "{}".into(),
            },
        }]);
        let response = hermix_core::LlmResponse {
            message: msg,
            usage: None,
            model: "test".into(),
            finish_reason: Some("tool_calls".into()),
        };
        let mut cost_guard = CostGuard::default();
        let mut message_cache = MessageCache::default();
        let action = agent
            .process_post_response(
                &mut ctx,
                &response,
                None,
                0,
                0, // api_elapsed
                &mut cost_guard,
                &mut message_cache,
                false,
            )
            .unwrap();
        assert!(matches!(action, PostResponseAction::Continue));
        // last_content_with_tools untouched (set later by the loop after
        // the assistant message is appended to ctx).
        assert!(message_cache.last_content_with_tools.is_none());
    }

    #[tokio::test]
    async fn resolve_pre_dispatch_allow_when_hook_unset() {
        // No hook installed → resolve_pre_dispatch returns an empty map and
        // never blocks.
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        );
        let tcs = vec![ToolCall {
            id: "tc1".into(),
            function: hermix_core::FunctionCall {
                name: "read_file".into(),
                arguments: "{}".into(),
            },
        }];
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        assert!(decisions.is_empty());
    }

    #[tokio::test]
    async fn resolve_pre_dispatch_routes_through_hook() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = calls.clone();
        let callbacks = AgentCallbacks {
            on_pre_tool_dispatch: Some(Box::new(move |tc: &ToolCall| {
                calls_clone.fetch_add(1, Ordering::SeqCst);
                if tc.function.name == "dangerous_tool" {
                    ToolDispatchDecision::Deny {
                        reason: "blocked by test".to_string(),
                    }
                } else {
                    ToolDispatchDecision::Allow
                }
            })),
            ..Default::default()
        };
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_callbacks(callbacks);
        let tcs = vec![
            ToolCall {
                id: "a".into(),
                function: hermix_core::FunctionCall {
                    name: "read_file".into(),
                    arguments: "{}".into(),
                },
            },
            ToolCall {
                id: "b".into(),
                function: hermix_core::FunctionCall {
                    name: "dangerous_tool".into(),
                    arguments: "{}".into(),
                },
            },
        ];
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(decisions.get("a").unwrap().is_ok());
        assert_eq!(
            decisions.get("b").unwrap().as_ref().err().unwrap(),
            "blocked by test"
        );
    }

    #[tokio::test]
    async fn resolve_pre_dispatch_await_approval_blocks_then_resolves() {
        let callbacks = AgentCallbacks {
            on_pre_tool_dispatch: Some(Box::new(|_tc: &ToolCall| {
                let (_tx, rx) = tokio::sync::oneshot::channel::<bool>();
                ToolDispatchDecision::AwaitApproval(rx)
            })),
            ..Default::default()
        };
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_callbacks(callbacks);
        let tcs = vec![ToolCall {
            id: "x".into(),
            function: hermix_core::FunctionCall {
                name: "x".into(),
                arguments: "{}".into(),
            },
        }];
        // Drop the spawned sender right away → receiver yields Err, hook
        // converts that into "Approval channel closed" denial.
        let decisions = agent.resolve_pre_dispatch(&tcs).await;
        let err = decisions.get("x").unwrap().as_ref().err().unwrap();
        assert!(err.contains("closed"), "unexpected err: {err}");
    }

    // -----------------------------------------------------------------------
    // M1.6.2: push/pop pair — `note_delegation_start` is wired into the
    // tool-dispatch paths and `note_delegation_end` matches by empty id
    // (topmost Delegation). Verify the pair generates exactly one
    // FocusConclusion per delegation.
    // -----------------------------------------------------------------------

    #[test]
    fn m162_delegation_push_pop_pair_emits_one_conclusion() {
        use hermix_focus::{FocusStack, SessionFrame, SessionFrameStatus};

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())));

        // session root
        agent.note_session_started("parent turn");
        // push with empty sub_agent_id (matches dispatch path behaviour
        // — the real sub_agent_id is parsed later from tool result content)
        let fid = agent
            .note_delegation_start("sub-task A", "")
            .expect("push succeeds");
        assert!(fid.is_nil() == false, "frame id is non-nil");

        // Stack depth should be 2 (root + delegation).
        let stack_depth = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(stack_depth, 2, "expected root + delegation");

        // Pop with empty id → matches topmost Delegation.
        let popped_depth = agent
            .note_delegation_end("", SessionFrameStatus::Completed)
            .expect("pop succeeds");
        // `note_delegation_end` returns the frame's *payload* depth
        // field (delegate_depth counter, not stack depth). The default
        // agent has delegate_depth=0, so the popped frame's depth is 0.
        // Stack depth assertion happens below.
        assert_eq!(popped_depth, 0, "popped frame's depth field");

        // Stack should now have 1 frame (just root).
        let stack_depth_after = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(stack_depth_after, 1);

        // Drain conclusions — expect exactly one.
        // (drain_focus_conclusions_to_memory_router returns 0 when
        // memory_router is None, so check the focus stack directly.)
        let drained = agent
            .focus_stack
            .lock()
            .expect("lock")
            .drain_conclusions()
            .len();
        assert_eq!(drained, 1, "push/pop pair should emit exactly one conclusion");
    }

    #[test]
    fn m162_push_idempotent_when_session_root_already_present() {
        use hermix_focus::{FocusStack, SessionFrame};

        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())));

        // Calling note_session_started twice should be idempotent (no duplicate root).
        agent.note_session_started("first");
        agent.note_session_started("second");
        let depth = agent
            .focus_stack
            .lock()
            .expect("lock")
            .frames()
            .len();
        assert_eq!(depth, 1, "idempotent — only one root frame");
    }

    #[tokio::test]
    async fn drain_focus_conclusions_persists_to_memory_router() {
        use hermix_focus::{EmbeddedMemoryBackend, FocusStack, MemoryRouter, SessionFrame};
        use std::sync::Mutex;

        // Create a temporary embedded backend
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("memory.db");
        let embedded = Arc::new(
            EmbeddedMemoryBackend::open(&db_path).expect("open embedded backend"),
        );
        let router = Arc::new(MemoryRouter::new_with_embedded(
            vec![embedded.clone()],
            embedded.clone(),
        ));

        // Create agent with focus stack and memory router
        let agent = AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
        .with_focus_stack(Arc::new(Mutex::new(FocusStack::<SessionFrame>::new())))
        .with_memory_router(router);

        // Push a delegation frame and pop it to generate a conclusion
        agent.note_session_started("test-session");
        agent.note_delegation_start("test task summary", "sub-agent-1");
        agent.note_delegation_end("sub-agent-1", hermix_focus::SessionFrameStatus::Completed);

        // Drain conclusions
        let count = agent.drain_focus_conclusions_to_memory_router();
        assert!(count >= 1, "should drain at least one conclusion");

        // Wait for async write
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Verify the conclusion was persisted - search for conclusion tag
        let query = hermix_focus::MemoryQuery::recall("conclusion");
        let (hits, _) = embedded.recall(&query).await.unwrap();
        assert!(!hits.is_empty(), "conclusion should be retrievable from embedded backend");
    }

    // -- M5 ACUI emit tests ---------------------------------------------

    /// Helper: build a minimal agent with no callbacks wired.
    fn bare_agent() -> AgentLoop {
        AgentLoop::new(
            AgentConfig::default(),
            Arc::new(ToolRegistry::new()),
            Arc::new(stub_provider::EmptyProvider),
        )
    }

    #[test]
    fn m5_emit_acui_no_callback_is_noop() {
        // Without `on_acui_directive` wired, emit_acui should silently drop.
        let agent = bare_agent();
        agent.emit_acui_show("c1", "weather", serde_json::json!({"city": "Beijing"}));
        agent.emit_acui_update("c1", serde_json::json!({"temp": 23}));
        agent.emit_acui_hide("c1");
        // No panic, no return value to assert.
    }

    #[test]
    fn m5_emit_acui_show_invokes_callback_with_json() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        // We own the only Arc to `agent.callbacks` here, so get_mut
        // succeeds and we can install the callback directly.
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_show("c1", "weather", serde_json::json!({"city": "Beijing"}));
        let got = captured.lock().unwrap();
        assert_eq!(got.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "show");
        assert_eq!(v["type"], "weather");
        assert_eq!(v["id"], "c1");
        assert_eq!(v["props"]["city"], "Beijing");
        assert_eq!(v["position"], "after");
    }

    #[test]
    fn m5_emit_acui_update_serializes_id_and_patch() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_update("card-7", serde_json::json!({"step": 5}));
        let got = captured.lock().unwrap();
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "update");
        assert_eq!(v["id"], "card-7");
        assert_eq!(v["patch"]["step"], 5);
    }

    #[test]
    fn m5_emit_acui_hide_serializes_id() {
        use std::sync::Mutex as StdMutex;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let cap_clone = captured.clone();

        let mut agent = bare_agent();
        let cb: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |payload: &str| {
            cap_clone.lock().unwrap().push(payload.to_string());
        });
        Arc::get_mut(&mut agent.callbacks)
            .expect("callbacks Arc should be uniquely owned in test")
            .on_acui_directive = Some(cb);

        agent.emit_acui_hide("c1");
        let got = captured.lock().unwrap();
        let v: serde_json::Value = serde_json::from_str(&got[0]).unwrap();
        assert_eq!(v["op"], "hide");
        assert_eq!(v["id"], "c1");
    }
}

/// Lightweight stub provider for `resolve_pre_dispatch` tests. Implements the
/// minimum `LlmProvider` surface so we can construct an `AgentLoop` without
/// pulling a real provider into a test.
#[cfg(test)]
mod stub_provider {
    use super::*;
    use async_trait::async_trait;
    use futures::stream::{BoxStream, StreamExt};
    use hermix_core::{AgentError, LlmProvider, LlmResponse, Message, StreamChunk, ToolSchema};

    pub struct EmptyProvider;

    #[async_trait]
    impl LlmProvider for EmptyProvider {
        async fn chat_completion(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> Result<LlmResponse, AgentError> {
            Ok(LlmResponse {
                message: Message::assistant(""),
                usage: None,
                model: "stub".into(),
                finish_reason: Some("stop".into()),
            })
        }

        fn chat_completion_stream(
            &self,
            _messages: &[Message],
            _tools: &[ToolSchema],
            _max_tokens: Option<u32>,
            _temperature: Option<f64>,
            _model: Option<&str>,
            _extra_body: Option<&serde_json::Value>,
        ) -> BoxStream<'static, Result<StreamChunk, AgentError>> {
            futures::stream::empty().boxed()
        }
    }

    fn make_reasoning_response(reasoning: &str) -> hermix_core::LlmResponse {
        let mut msg = Message::assistant("");
        msg.reasoning_content = Some(reasoning.to_string());
        hermix_core::LlmResponse {
            message: msg,
            usage: None,
            model: "gpt-4o".to_string(),
            finish_reason: None,
        }
    }

    fn make_empty_response() -> hermix_core::LlmResponse {
        hermix_core::LlmResponse {
            message: Message::assistant(""),
            usage: None,
            model: "gpt-4o".to_string(),
            finish_reason: None,
        }
    }

    fn make_loop_for_classify() -> (AgentLoop, ContextManager) {
        let config = AgentConfig::default();
        // `classify_post_llm_response` is a pure synchronous classifier
        // — it never touches the LLM provider, tool registry, or any
        // IO channel — so an empty stub provider and an empty registry
        // are enough to satisfy the `AgentLoop::new` constructor.
        let loop_ = AgentLoop::new(
            config,
            Arc::new(ToolRegistry::new()),
            Arc::new(EmptyProvider),
        );
        let ctx = ContextManager::new(100);
        (loop_, ctx)
    }

    #[test]
    fn p4_1_classify_breaks_on_tool_calls() {
        // Tool calls present → Break(r) regardless of content /
        // reasoning state. Caller unwraps the response and dispatches.
        let (loop_, mut ctx) = make_loop_for_classify();
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let mut msg = Message::assistant("");
        msg.tool_calls = Some(vec![hermix_core::ToolCall {
            id: "tc1".into(),
            function: hermix_core::FunctionCall {
                name: "noop".into(),
                arguments: "{}".into(),
            },
        }]);
        let action = loop_.classify_post_llm_response(
            hermix_core::LlmResponse {
                message: msg,
                usage: None,
                model: "gpt-4o".to_string(),
                finish_reason: None,
            },
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::Break(_)));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 0);
    }

    #[test]
    fn p4_1_classify_breaks_on_visible_text() {
        // No tools, but assistant content is non-empty → Break(r).
        // Counter budgets must not be touched.
        let (loop_, mut ctx) = make_loop_for_classify();
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            hermix_core::LlmResponse {
                message: Message::assistant("hello world"),
                usage: None,
                model: "gpt-4o".to_string(),
                finish_reason: None,
            },
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::Break(_)));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 0);
    }

    #[test]
    fn p4_1_classify_retries_reasoning_within_budget() {
        // Reasoning-only response with the budget set to 2 → first
        // call increments to 1/2 and returns RetryReasoning; the
        // message is pushed onto ctx so the next LLM call sees it as
        // prior context.
        let (mut loop_, mut ctx) = make_loop_for_classify();
        loop_.config.thinking_prefill_max_retries = 2;
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            make_reasoning_response("thinking…"),
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::RetryReasoning));
        assert_eq!(inner_thinking, 1);
        assert_eq!(inner_empty, 0);
        assert_eq!(ctx.get_messages().len(), 1);
    }

    #[test]
    fn p4_1_classify_retries_empty_within_budget() {
        // No tools, no reasoning, no content → RetryEmpty (not
        // RetryReasoning). The message must NOT be pushed — empty
        // responses are noise that shouldn't pollute the conversation.
        let (mut loop_, mut ctx) = make_loop_for_classify();
        loop_.config.empty_content_max_retries = 1;
        let mut inner_thinking = 0;
        let mut inner_empty = 0;
        let action = loop_.classify_post_llm_response(
            make_empty_response(),
            &mut inner_thinking,
            &mut inner_empty,
            &mut ctx,
        );
        assert!(matches!(action, PostLlmAction::RetryEmpty));
        assert_eq!(inner_thinking, 0);
        assert_eq!(inner_empty, 1);
        assert_eq!(ctx.get_messages().len(), 0);
    }
}
