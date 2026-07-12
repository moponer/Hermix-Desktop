import type { ChatMessage, ExecutionTimelineEvent, TimelineStep } from "../types";

export interface ParsedTask {
  done: boolean;
  text: string;
}

export interface ParsedContent {
  body: string;
  tasks: ParsedTask[];
}

// ── Thinking inline ──────────────────────────────────────────────
//
// The system prompt instructs the model to emit reasoning inside
// `<think>...</think>`. We drop these blocks entirely from the
// rendered body — they're internal scaffolding, not user output.
// Only tool calls, file edits, approval requests and task lists get
// special visual treatment.
const THINKING_RE = /<think>[\s\S]*?<\/think>/gi;
const ORPHAN_OPEN_RE = /<think>[\s\S]*$/i;

// Server lifecycle / status logs that occasionally leak into the
// assistant body.  These are framework-level diagnostics, not user
// content, so we strip them silently.
const LIFECYCLE_RE = /\[lifecycle:[^\]]*\]/gi;

// ── Context-compaction banner (defence-in-depth) ─────────────────
//
// `ContextCompressor::compress()` prepends `SUMMARY_PREFIX`
// ("[CONTEXT COMPACTION] Earlier turns in this conversation were
// compacted …") to every compaction summary, and `LocalAgentService`
// injects a sibling "[CONTEXT COMPACTION — REFERENCE ONLY]" system
// message when a session is reloaded. Both are infrastructure that
// the *LLM* needs to see — they're not user-facing content.
//
// Two failure modes leak the banner into the chat:
//
//   1. The LLM echoes the banner verbatim as the opening of its
//      reply. This is the symptom the user reported: they sent
//      "你好", and the AI's "reply" was the [CONTEXT COMPACTION]
//      block. (The banner's own prompt explicitly says "Do NOT echo,
//      quote or re-issue" — but that instruction is best-effort.)
//
//   2. A persisted assistant message from a pre-fix turn was loaded
//      on session reload and rendered as-is.
//
// Without this strip the user sees a 1-2 KB wall of system-level
// scaffolding where their reply should be. Match covers the two
// banner variants:
//
//   - `[CONTEXT COMPACTION]`            — current `SUMMARY_PREFIX`
//   - `[CONTEXT COMPACTION — REFERENCE ONLY]` — session-reload variant
//   - `[CONTEXT SUMMARY]:`              — legacy v1 prefix
//                                          (`compression.rs:55`
//                                          `LEGACY_SUMMARY_PREFIX`)
//
// Pattern requires the closing `--- END OF … ---` marker on its own
// line. Without the marker we leave the banner alone — false negatives
// here are acceptable (the user sees a banner they probably should
// see) while false positives (silently eating user content) are not.
// The reference-only reload banner has no closing marker by design, so
// it falls through this filter and is handled by `MessageBubble` not
// rendering `role: "system"` rows in the first place.
const CONTEXT_COMPACTION_RE =
  /\[CONTEXT (?:COMPACTION[^\]]*|SUMMARY)\]:?[\s\S]*?\n---\s*END OF[\s\S]*?---/gi;

// ── Tool-call XML leak (defence-in-depth) ─────────────────────────
//
// Some open-weight models (MiniMax / M2.7, MiniMax / Hermes fine-tunes)
// emit tool calls as raw XML inside their normal text content instead
// of through the OpenAI `tools` field. The server has a streaming
// redactor that strips these blocks at three layers (LLM chunk → WS
// payload → persisted message), but the LAST line of defence is the
// renderer. If anything slipped through (stale `hermix.exe` sidecar
// from before the redactor was added, a persisted message from a
// pre-fix turn, or a non-streaming path we haven't audited), the user
// still never sees leaked XML — `parseContent` removes it here.
//
// Patterns we strip:
//   1. Full MiniMax wrapper:    <minimax:tool_call>…</minimax:tool_call>
//   2. Bare prefix + invoke:    minimax:tool_call <invoke …>…</invoke>
//   3. Standalone <invoke…>…</invoke>  (Hermes standard, no wrapper)
//   4. Orphan close tags       </minimax:tool_call>  </invoke>
const TOOL_CALL_FULL_RE =
  /<minimax:tool_call\s*>[\s\S]*?<\/minimax:tool_call>/gi;
// Bare "minimax:tool_call" (no leading `<`) immediately followed by
// whitespace and a `<invoke …>` block. We require the `<` to follow
// within ~32 chars, otherwise the literal is just prose mention.
const TOOL_CALL_BARE_OPEN_RE =
  /(^|[^\w])minimax:tool_call(?=[\s\S]{0,32}<\s*invoke\b)/gi;
const INVOKE_BLOCK_RE = /<\s*invoke\s+name\s*=\s*"[^"]*"\s*>[\s\S]*?<\s*\/\s*invoke\s*>/gi;
const ORPHAN_CLOSE_RE = /<\s*\/\s*(?:minimax:tool_call|invoke)\s*>/gi;
const BARE_MINIMAX_LITERAL_RE =
  /(^|[^\w])minimax:tool_call(?![:\w])/gi;

export function parseContent(content: string): ParsedContent {
  let working = content;

  // Drop closed <think> blocks entirely.
  working = working.replace(THINKING_RE, "");

  // Drop orphan (unclosed) <think> tags the same way.
  if (working.match(ORPHAN_OPEN_RE)) {
    working = working.replace(ORPHAN_OPEN_RE, "");
  }

  // Drop [lifecycle: ...] status noise.
  working = working.replace(LIFECYCLE_RE, "");

  // Drop [CONTEXT COMPACTION] / [CONTEXT SUMMARY]: banners.
  // Backend infrastructure for the LLM, never user-facing. See the
  // CONTEXT_COMPACTION_RE constant for the two failure modes this
  // guards against (LLM echo + pre-fix persisted message).
  working = working.replace(CONTEXT_COMPACTION_RE, "");

  // ── Tool-call XML leak (defence-in-depth) ──────────────────────
  // Order matters: strip the full MiniMax wrapper first, then bare
  // prefix + invoke blocks, then any orphan `<invoke>` blocks, then
  // orphan close tags, then any remaining bare `minimax:tool_call`
  // literal. Each pass uses a fresh regex (the `g` flag is
  // stateful) so they don't interfere.
  working = working.replace(TOOL_CALL_FULL_RE, "");
  working = working.replace(TOOL_CALL_BARE_OPEN_RE, "$1");
  working = working.replace(INVOKE_BLOCK_RE, "");
  working = working.replace(ORPHAN_CLOSE_RE, "");
  working = working.replace(BARE_MINIMAX_LITERAL_RE, "$1");

  // Extract checkbox tasks before trimming.
  const tasks: ParsedTask[] = [];
  const CHECKBOX_RE = /^[ \t]*[-*][ \t]+\[([ xX])\][ \t]+(.+?)[ \t]*$/gm;
  working = working.replace(CHECKBOX_RE, (_m, mark: string, text: string) => {
    tasks.push({ done: mark.toLowerCase() === "x", text: text.trim() });
    return "";
  });

  // Collapse runs of empty lines left behind by stripped blocks.
  working = working.replace(/\n{3,}/g, "\n\n").trim();

  return { body: working, tasks };
}

// ── Tool timeline grouping (mirrors ToolEventCard but normalized for rendering) ──

export interface FileChangeEntry {
  path: string;
  changeType: "created" | "modified" | "deleted" | "renamed";
  summary?: string;
  timestamp: string;
}

export interface ToolGroup {
  key: string;
  tool: string;
  kind: "tool" | "status";
  status: "running" | "done" | "error";
  events: ExecutionTimelineEvent[];
  startedAt?: string;
  endedAt?: string;
  durationMs?: number;
  /**
   * Set when a `status` event was attached to a running tool group
   * (rather than rendered as its own row). The rail renderer can
   * surface this next to the "执行中" badge so the user still sees
   * the LLM's progress note ("正在处理", "读取中…") without an
   * extra card.
   */
  statusNote?: string;
}

export function groupTimeline(events: ExecutionTimelineEvent[]): ToolGroup[] {
  const groups: ToolGroup[] = [];
  const active = new Map<string, number>();

  for (const ev of events) {
    if (ev.type === "file_change") {
      // file_change is metadata, not a tool run — collect separately.
      continue;
    }
    if (ev.type === "status") {
      // Attach the status note to the most recent *running* tool
      // group instead of rendering it as its own row. Without this,
      // a single tool call expands to 3 stacked rows in the rail:
      //   [tool card: read_file  执行中]
      //   [status note: 正在处理 …]
      //   [tool card: read_file  已执行]
      // The middle one is redundant — the user can already see
      // "执行中" on the tool card itself. Mirrors `groupEvents` in
      // ToolEventCard.tsx so the ACUI renderer and the inline
      // timeline stay in lock-step.
      const note = (ev.content ?? "").trim();
      if (!note) continue;
      // Walk back from the end to find the most recent still-running
      // tool group (matches the order in which the LLM emitted them).
      for (let i = groups.length - 1; i >= 0; i--) {
        const g = groups[i];
        if (g.kind === "tool" && g.status === "running") {
          g.statusNote = note;
          break;
        }
      }
      // Orphan status notes (no running tool to attach to) are
      // intentionally dropped — they were the source of the
      // "正在思考" / "初始化完成" floating cards.
      continue;
    }
    const tool = ev.tool?.trim() || "tool";
    // Use `tool_call_id` as the dedup key when the server provides one.
    // Falling back to `tool` (the name) groups concurrent calls to the
    // same handler — that was the source of the "browser_navigate ×4"
    // repeat row bug when the server retried/emitted the same event
    // twice: every event landed in the same `groups[0]`, and the
    // streaming renderer painted one card per `tool_complete` push.
    const dedupKey = (ev.tool_call_id && ev.tool_call_id.trim()) || tool;
    let idx = active.get(dedupKey);
    const fresh = ev.type === "tool_start" || idx === undefined;
    if (fresh) {
      groups.push({
        key: `${tool}-${groups.length}`,
        tool,
        kind: "tool",
        status: ev.type === "tool_complete" ? "done" : "running",
        events: [],
        startedAt: ev.created_at,
      });
      idx = groups.length - 1;
      active.set(dedupKey, idx);
    }
    const safeIdx = idx ?? groups.length - 1;
    const grp = groups[safeIdx];
    // De-dupe events: skip if this exact event object (same type+ts+content)
    // already landed in the same group from a prior replay. Belt-and-
    // suspenders for server-side retry paths that may re-emit
    // `tool_complete` with identical payload.
    const dup = grp.events.some(
      (prev) =>
        prev.type === ev.type &&
        prev.created_at === ev.created_at &&
        (prev.content ?? "") === (ev.content ?? ""),
    );
    if (!dup) {
      grp.events.push(ev);
    }
    if (ev.type === "tool_complete") {
      // Decide between "done" and "error" from the close-event
      // payload. Without this, every tool that exited non-zero
      // would render as a green "成功" pill on reload — masking
      // the fact that the command actually failed.
      const errReason = detectToolError(ev.content);
      grp.status = errReason ? "error" : "done";
      grp.endedAt = ev.created_at;
      if (errReason) grp.statusNote = errReason;
      if (grp.startedAt) {
        grp.durationMs = Math.max(0, new Date(grp.endedAt).getTime() - new Date(grp.startedAt).getTime());
      }
      active.delete(dedupKey);
    }
  }
  // Stranded pass — any group still in `active` had a `tool_start`
  // but never received a `tool_complete`. On reload (when the live
  // WS is long gone) the right read is "interrupted", not "still
  // running". The previous behaviour left these groups in their
  // default `"running"` state, which the renderer then painted as
  // a green "运行中" badge — the exact symptom the user reported
  // ("重启进入对话,所有工具卡在运行中,还多了一个 terminal
  // 运行中子行"). Promote each stranded group to `"error"` with
  // a short note; the renderer's existing error branch handles
  // badge + LiveTerminal label from there.
  for (const idx of active.values()) {
    const grp = groups[idx];
    if (grp && grp.status === "running") {
      grp.status = "error";
      grp.statusNote = "执行已中断（无完成事件）";
    }
  }
  return groups;
}

/**
 * Inspect a `tool_complete` payload (or a final stdout chunk from a
 * tool that never emitted a complete event) and decide whether the
 * invocation ended in error. Returns a short human-readable reason
 * when an error is detected, or `null` when the call looks healthy.
 *
 * Three error shapes we recognise, in priority order:
 *   1. JSON wrapper with `exit_code !== 0` and either `error` or
 *      `stderr` non-empty   — terminal / shell / execute_command
 *   2. JSON wrapper with top-level `error` field
 *      — generic tool errors propagated by the agent runtime
 *   3. Plain-text payloads that begin with `Failed` / `Error` /
 *      `Tool execution failed` / `ERR`   — last-ditch heuristic
 *
 * Why we duplicate this in two places: `tool_complete.content` is
 * the canonical payload but `groupTimeline` needs to decide on the
 * raw `content` string (possibly empty for tools that error before
 * writing any output), while `expandTimelineToMessages` wants to
 * derive the same answer for the *expanded* per-call message. The
 * two share the exact same logic via this helper.
 */
export function detectToolError(content?: string): string | null {
  if (!content) return null;
  const trimmed = content.trim();
  if (!trimmed) return null;
  // Plain-text error prefix heuristic.
  if (/^(Failed|Error|ERR|Tool execution failed)/i.test(trimmed)) {
    return trimmed.length > 120 ? trimmed.slice(0, 117) + "…" : trimmed;
  }
  if (!trimmed.startsWith("{")) return null;
  try {
    const parsed = JSON.parse(trimmed);
    // 1. {"error":"..."}  (top-level)
    if (typeof parsed.error === "string" && parsed.error.trim().length > 0) {
      const e = parsed.error;
      return e.length > 120 ? e.slice(0, 117) + "…" : e;
    }
    // 2. {"stderr":"...","exit_code":1}  (Bash / execute_command)
    if (
      typeof parsed.exit_code === "number" &&
      parsed.exit_code !== 0 &&
      typeof parsed.stderr === "string" &&
      parsed.stderr.trim().length > 0
    ) {
      const e = parsed.stderr;
      return e.length > 120 ? e.slice(0, 117) + "…" : e;
    }
    // 3. {"exit_code":1}  alone — surface the non-zero exit.
    if (typeof parsed.exit_code === "number" && parsed.exit_code !== 0) {
      return `exit code ${parsed.exit_code}`;
    }
    return null;
  } catch {
    return null;
  }
}

/**
 * Expand a list of server-returned messages so that every assistant
 * message whose `execution_timeline` contains tool events gets
 * converted into an interleaved sequence of (assistant → tool → ...
 * → tool → assistant) messages.
 *
 * This is the "reload" counterpart of the live-stream behaviour in
 * `App.tsx` where `onToolStart` / `onToolComplete` insert real
 * `role: "tool"` messages at runtime. Without this step, a session
 * reload renders every tool call as a collapsed appendix inside the
 * assistant bubble — losing the inline interleaving the user saw
 * during the live stream.
 */
export function extractFileChanges(events?: ExecutionTimelineEvent[]): FileChangeEntry[] {
  if (!events) return [];
  const out: FileChangeEntry[] = [];
  const seen = new Set<string>();
  for (const ev of events) {
    if (ev.type !== "file_change" || !ev.file_path) continue;
    const key = `${ev.change_type ?? "modified"}::${ev.file_path}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({
      path: ev.file_path,
      changeType: (ev.change_type ?? "modified") as FileChangeEntry["changeType"],
      summary: ev.file_summary,
      timestamp: ev.created_at,
    });
  }
  return out;
}

/**
 * Canonicalize a JSON args string the same way the backend's
 * `tool_call_dedup_key` does (parse + re-serialize with sorted keys;
 * fall back to the trimmed raw string if it isn't valid JSON). Used to
 * match a standalone `role:"tool"` row against an inline tool card
 * expanded from an execution_timeline so duplicate tool results are
 * skipped on session reload.
 */
function serdeJsonNorm(args: string): string {
  try {
    const v = JSON.parse(args);
    return JSON.stringify(v);
  } catch {
    return args.trim();
  }
}

export function expandTimelineToMessages(messages: ChatMessage[]): ChatMessage[] {
  const out: ChatMessage[] = [];
  // Track assistant IDs whose timeline was expanded into inline
  // role:"tool" messages — any original role:"tool" that immediately
  // follows such an assistant should be skipped to prevent duplicates.
  const expandedAssistantIds = new Set<string>();
  // Signatures (tool name + normalized args) of the inline tool cards we
  // expanded out of an assistant's execution_timeline. A standalone
  // role:"tool" row that the server persisted *in addition to* the
  // timeline carries the identical (tool, args) and would otherwise
  // render as a SECOND card — the "工具结果出现两次" the user saw. We
  // skip any standalone tool whose signature collides with an already
  // expanded inline tool.
  const expandedToolSigs = new Set<string>();

  const toolSig = (tool: string | undefined, args: string | undefined): string => {
    const name = (tool ?? "").toLowerCase();
    let norm = "";
    if (args && args.trim()) {
      norm = serdeJsonNorm(args);
    }
    return `${name}:${norm}`;
  };

  for (const msg of messages) {
    if (msg.role !== "assistant" || !msg.execution_timeline || msg.execution_timeline.length === 0) {
      // Skip original role:"tool" messages that duplicate a tool we
      // already expanded from a preceding assistant's execution_timeline.
      if (msg.role === "tool") {
        const sig = toolSig(msg.tool?.tool, msg.tool?.args);
        if (expandedToolSigs.has(sig)) {
          continue;
        }
      }
      out.push(msg);
      continue;
    }

    const timeline = msg.execution_timeline;
    const groups = groupTimeline(timeline);

    if (groups.length === 0) {
      out.push(msg);
      continue;
    }

    // Mark this assistant as expanded so subsequent role:"tool" messages
    // from the DB (which duplicate the timeline data) are skipped.
    expandedAssistantIds.add(msg.id);

    // Per-group tool messages. Strip `execution_timeline` from the
    // outgoing assistant message: the tool groups are now expanded
    // into inline `role: "tool"` messages below, so leaving the
    // original timeline attached would cause `AssistantMessage` to
    // render `ToolTimelineBlock` *in addition to* the inline cards —
    // i.e. two cards per tool call, which is what the user
    // complained about ("一件事三行").
    out.push({ ...msg, execution_timeline: undefined });

    for (const g of groups) {
      if (g.kind === "status") {
        out.push({
          id: `${msg.id}-status-${g.key}`,
          role: "assistant",
          content: g.events[0]?.content ?? "",
          timestamp: g.startedAt ?? msg.timestamp,
        });
        continue;
      }

      const startEv = g.events.find((e) => e.type === "tool_start");
      const completeEv = g.events.find((e) => e.type === "tool_complete");
      const stdout = g.events
        .filter((e) => e.type === "tool_stdout")
        .map((e) => e.content ?? "")
        .join("");

      // Record this inline tool's signature so the server's standalone
      // role:"tool" row (persisted in addition to the timeline) is skipped
      // as a duplicate below.
      expandedToolSigs.add(toolSig(g.tool, startEv?.arguments));

      out.push({
        id: `${msg.id}-tool-${g.key}`,
        role: "tool",
        content: completeEv?.content ?? "",
        timestamp: g.startedAt ?? msg.timestamp,
        tool: {
          tool: g.tool,
          args: startEv?.arguments,
          // `g.status` already encodes the truth: "running" while
          // a tool_call_id is still open, "error" if the close
          // event carried an error payload (see `groupTimeline`
          // comments above), "done" otherwise. Previously this
          // line collapsed everything-but-running to "done", which
          // is why reloads painted a green "运行中" badge on
          // tools that had clearly errored (terminal exit ≠ 0).
          status: g.status,
          // Stash a one-line reason on the meta so the renderer
          // can show "已中断" / "失败" instead of the generic
          // "执行中" pill. The renderer reads this via
          // `meta.errorReason`; older versions of the renderer
          // will simply ignore the field.
          errorReason: g.statusNote?.startsWith("执行已中断")
            ? g.statusNote
            : g.status === "error"
              ? g.statusNote ?? "执行失败"
              : undefined,
          output: stdout,
          startedAt: g.startedAt ?? msg.timestamp,
          completedAt: g.endedAt,
          linkedAssistantId: msg.id,
        },
      });
    }
  }

  return out;
}

export interface ResolvedMessage {
  thinking: string | null;
  body: string;
  tasks: ParsedTask[];
  timeline: ToolGroup[];
  toolCalls: NonNullable<ChatMessage["tool_calls"]>;
  backend: ChatMessage["execution_backend"];
  model: string | undefined;
}

/**
 * Build a unified timeline step list from the legacy
 * `reasoningContent` / `execution_timeline` / `content` fields.
 *
 * Used as a fallback when `message.unified_steps` is not available
 * (e.g. old persisted messages that pre-date the unified timeline
 * feature). Produces a simple 3-step list:
 *
 *   [reasoning?] → [tools?] → [text?]
 *
 * For messages that already have `unified_steps` (set by
 * `mergeConsecutiveAssistantMessages` or the streaming handlers),
 * the caller should use those directly instead of calling this.
 */
export function buildFallbackSteps(message: ChatMessage): TimelineStep[] {
  const steps: TimelineStep[] = [];
  const reasoning = (message.reasoningContent ?? "").replace(/<\/?think>/gi, "").trim();
  // Filter out non-tool events (status, file_change, section_gate) —
  // those are handled separately by the renderer (file changes summary,
  // etc.). Only tool_* events belong in the tools step.
  const toolEvents = (message.execution_timeline ?? []).filter(
    (e) => e.type === "tool_start" || e.type === "tool_stdout" || e.type === "tool_complete",
  );
  const parsed = parseContent(message.content);
  const hasBody = !!(parsed.body && parsed.body.trim());

  // 2026-07-11: When the LLM doesn't close its <think> tag, the
  // ThinkBlockParser routes ALL text (including body) to
  // reasoning_content, leaving content empty. On reload, we need
  // to show that text as body — NOT as a reasoning block — to
  // match what the user saw during streaming (the onDone handler
  // uses reply_text which falls back to reasoning_content).
  //
  // Rule: if content has body text, show reasoning as a reasoning
  // block + body as text (normal case). If content is empty but
  // reasoning has text, show reasoning AS the body text (fallback
  // case) — don't also show it as a reasoning block to avoid
  // duplication.
  if (hasBody) {
    if (reasoning) {
      steps.push({ kind: "reasoning", content: reasoning });
    }
    if (toolEvents.length > 0) {
      steps.push({ kind: "tools", events: toolEvents });
    }
    steps.push({ kind: "text", content: parsed.body });
  } else if (reasoning) {
    // Fallback: content is empty, reasoning has the text.
    // Show it as body text, not as a reasoning block.
    if (toolEvents.length > 0) {
      steps.push({ kind: "tools", events: toolEvents });
    }
    steps.push({ kind: "text", content: reasoning });
  } else {
    if (toolEvents.length > 0) {
      steps.push({ kind: "tools", events: toolEvents });
    }
  }
  return steps;
}

/**
 * Resolve the effective unified steps for a message: use
 * `message.unified_steps` when available, otherwise fall back to
 * `buildFallbackSteps`.
 *
 * 2026-07-11: Adjacent `kind: "reasoning"` steps are merged into
 * a single block. The LLM often emits several `<think>…</think>`
 * segments back-to-back (one per sub-topic, separated by `**小标题**`
 * + blank lines). Server emits one `StreamEvent::Thinking` per
 * segment, which previously rendered as N collapsible blocks in the
 * chat UI ("思考被切成两段"). Merging adjacent reasoning preserves
 * each segment's text (joined by `\n\n`) but shows them as one
 * collapsed reasoning block, matching the user's intuition that a
 * continuous monologue is one thought process.
 */
export function resolveSteps(message: ChatMessage): TimelineStep[] {
  const raw =
    message.unified_steps && message.unified_steps.length > 0
      ? message.unified_steps
      : buildFallbackSteps(message);
  return mergeAdjacentReasoning(raw);
}

/**
 * Fold runs of consecutive `{kind: "reasoning"}` steps into one,
 * joined with `\n\n`. Stops at any non-reasoning step (tools, real
 * text). A "think-block residue" text step — short markdown headings
 * or stray think tags that the LLM emits *between* adjacent think
 * blocks (e.g. `**Clarifying directory paths**` between two <think>
 * spans) — is also absorbed so the chat UI shows one continuous
 * reasoning block instead of N tiny ones.
 *
 * Stops at any tool step. Reasoning segments actually separated by a
 * tool call stay separate — those represent distinct turns of thought.
 */
export function mergeAdjacentReasoning(steps: TimelineStep[]): TimelineStep[] {
  if (steps.length < 2) return steps;
  const out: TimelineStep[] = [];
  let buf: string | null = null;
  for (const step of steps) {
    if (step.kind === "reasoning") {
      buf = buf === null ? step.content : buf + "\n\n" + step.content;
      continue;
    }
    if (step.kind === "text" && isThinkBlockResidue(step.content)) {
      // Drop the residue silently — it gets folded into the surrounding
      // reasoning buffer at flush time. This is the key fix for
      // "思考被切成多段" when the LLM outputs multiple adjacent think
      // blocks with short markdown headings between them.
      continue;
    }
    if (buf !== null) {
      out.push({ kind: "reasoning", content: buf });
      buf = null;
    }
    out.push(step);
  }
  if (buf !== null) {
    out.push({ kind: "reasoning", content: buf });
  }
  return out;
}

/**
 * Detect the "LLM-emitted headings between adjacent think blocks"
 * pattern. Examples that should return true:
 *   - `**Clarifying directory paths**`
 *   - `### Identifying file structure`
 *   - `<think>` or `</think>` (stray tags, in case any leak through)
 *   - whitespace only
 * Examples that should return false (real user-visible text):
 *   - `这里是回答正文`
 *   - `The file contains 42 lines.`
 *
 * Heuristic but tight: short content (< 200 chars trimmed) AND
 * either only bold-markdown spans, only ATX headings, or only
 * stray think-tag fragments.
 */
function isThinkBlockResidue(s: string): boolean {
  const t = s.trim();
  if (t.length === 0) return true;
  if (t.length > 200) return false;
  if (/^<\/?think>?$/i.test(t)) return true;
  // One or more `**word(s)**` spans separated by whitespace/newlines
  if (/^(\*\*[^*\n]{1,80}\*\*\s*)+$/.test(t)) return true;
  // ATX-style heading(s): `# ...`, `## ...`, etc.
  if (/^(#{1,6}\s+\S.{0,160})+$/.test(t)) return true;
  return false;
}
