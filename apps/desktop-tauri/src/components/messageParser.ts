import type { ChatMessage, ExecutionTimelineEvent, TimelineStep } from "../types";
import { classifyToolOutcome, detectToolError } from "../utils/toolOutcome";

// 结局判定的实现已搬进 `utils/toolOutcome.ts`（零依赖），这里是转发，
// 保持既有 import 路径可用。运行态 / 恢复态 / ACUI 三条路径现在共用
// 同一份实现，不再各抄一遍靠注释同步。
export { classifyToolOutcome, detectToolError };

export interface ParsedTask {
  done: boolean;
  text: string;
}

export interface ParsedContent {
  body: string;
  tasks: ParsedTask[];
}

// ── Thinking inline ──────────────────────────────────────────────
// The system prompt instructs the model to emit reasoning inside
// `<think>...</think>`. We drop these blocks entirely from the
// rendered body — they're internal scaffolding, not user output.
// Some models emit `<reasoning>...</reasoning>` instead; treat it
// the same way so the user never sees raw reasoning tags.
// Only tool calls, file edits, approval requests and task lists get
// special visual treatment.
const THINKING_RE = /<think>[\s\S]*?<\/think>/gi;
const ORPHAN_OPEN_RE = /<think>[\s\S]*$/i;
const REASONING_RE = /<reasoning>[\s\S]*?<\/reasoning>/gi;
const ORPHAN_REASONING_RE = /<reasoning>[\s\S]*$/i;

// Server lifecycle / status logs that occasionally leak into the
// assistant body.  These are framework-level diagnostics, not user
// content, so we strip them silently.
const LIFECYCLE_RE = /\[lifecycle:[^\]]*\]/gi;

// ── Context-compaction banner (defence-in-depth) ─────────────────
// `ContextCompressor::compress()` prepends `SUMMARY_PREFIX`
// ("[CONTEXT COMPACTION] Earlier turns in this conversation were
// compacted …") to every compaction summary, and `LocalAgentService`
// injects a sibling "[CONTEXT COMPACTION — REFERENCE ONLY]" system
// message when a session is reloaded. Both are infrastructure that
// the *LLM* needs to see — they're not user-facing content.
// Two failure modes leak the banner into the chat: the LLM echoes it
// back as its "reply" (the banner's own prompt says "Do NOT echo,
// quote or re-issue", but that is best-effort), or it is injected as
// a sibling system message on session reload and rendered as-is.
// Without this strip the user sees a 1-2 KB wall of system-level
// scaffolding where their reply should be. Match covers the two
// banner variants:
// - `[CONTEXT COMPACTION]`            — current `SUMMARY_PREFIX`
// - `[CONTEXT COMPACTION — REFERENCE ONLY]` — session-reload variant
// (`compression.rs:55`
// `LEGACY_SUMMARY_PREFIX`)
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
// Some open-weight models (MiniMax / M2.7, MiniMax / Hermes fine-tunes)
// emit tool calls as raw XML inside their normal text content instead
// of through the OpenAI `tools` field. The server has a streaming
// redactor that strips these blocks at three layers (LLM chunk → WS
// payload → persisted message), but the LAST line of defence is the
// renderer. If anything slipped through (stale `hermix.exe` sidecar
// from before the redactor was added, a persisted message from a
// pre-fix turn, or a non-streaming path we haven't audited), the user
// still never sees leaked XML — `parseContent` removes it here.
// Patterns we strip:
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

  // Drop closed <reasoning> blocks (some providers use this tag instead of
  // <think>). Must happen before orphan handling so a closed block doesn't
  // get swallowed by the orphan regex.
  working = working.replace(REASONING_RE, "");

  // Drop orphan (unclosed) <reasoning> tags.
  if (working.match(ORPHAN_REASONING_RE)) {
    working = working.replace(ORPHAN_REASONING_RE, "");
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

  // Checklists remain ordinary Markdown in the assistant answer. A real
  // executable task plan is rendered only from the explicit `todo.*` event
  // stream (`TodoListCard`), never inferred from text formatting.
  const tasks: ParsedTask[] = [];

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
  /**
   * 四态而非三态。`skipped` = 工具**没有真正跑**（审批超时、用户拒绝）——
   * 既不是成功也不是失败。2026-10-01 之前这类被塞进 `done`，UI 打「✓已完成」，
   * 等于对一件没发生的事宣布成功。判定见 `utils/toolOutcome.ts`。
   */
  status: "running" | "done" | "error" | "skipped";
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

/**
 * Group timeline events into one `ToolGroup` per tool invocation.
 *
 * `opts.streamComplete` (default `true`): whether the *parent turn*
 * has finished streaming. While the live WS is still feeding events
 * (`streamComplete === false`), any group that opened but never
 * received a `tool_complete` is left in `"running"` state — the
 * renderer paints it as a "运行中" badge + spinner (see ToolRow),
 * which is the correct visual cue for "tool started, result
 * still arriving". Only once the parent turn settles do we promote
 * stranded groups to `"error"` with an "执行已中断（无完成事件）"
 * note — that path is reserved for genuine reload-after-crash cases
 * where the live WS is long gone.
 *
 * Default `true` keeps the legacy behaviour for any caller that
 * doesn't know about the option (notably `expandTimelineToMessages`
 * at the bottom of this file, which only ever runs on persisted
 * history after the stream is fully settled).
 */
export function groupTimeline(
  events: ExecutionTimelineEvent[],
  opts?: { streamComplete?: boolean },
): ToolGroup[] {
  const streamComplete = opts?.streamComplete ?? true;
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
      // [tool card: read_file  执行中]
      // [status note: 正在处理 …]
      // [tool card: read_file  已执行]
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
      // 结局判定是运行态 / 恢复态 / ACUI 三条路径的唯一入口
      // （`utils/toolOutcome.ts`）。`kind === null` 才是「完成」。
      const outcome = classifyToolOutcome(ev.content, ev.is_error);
      grp.status = outcome.kind ?? "done";
      grp.endedAt = ev.created_at;
      if (outcome.reason) grp.statusNote = outcome.reason;
      if (grp.startedAt) {
        grp.durationMs = Math.max(0, new Date(grp.endedAt).getTime() - new Date(grp.startedAt).getTime());
      }
      active.delete(dedupKey);
    }
  }
  // Stranded pass — only runs once the parent turn has settled.
  // During live streaming (`streamComplete === false`) we leave
  // any `tool_start`-without-`tool_complete` group in `"running"`
  // state so the renderer keeps showing the spinner / "运行中"
  // badge — the close event is simply on its way. Forcing these
  // to "error" mid-stream is what produced the "上来就提示执行中断,
  // On settled reload (the default `true`), we still promote every
  // stranded group to `"error"` + "执行已中断（无完成事件）" —
  // that is the correct read for history recovered after the live
  // WS is long gone. The renderer's existing error branch handles
  // the badge + LiveTerminal label from there.
  if (streamComplete) {
    for (const idx of active.values()) {
      const grp = groups[idx];
      if (grp && grp.status === "running") {
        grp.status = "error";
        grp.statusNote = "执行已中断（无完成事件）";
      }
    }
  }
  return groups;
}

// 实现见 `utils/toolOutcome.ts`（已在上方 import 并转发）。
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

/** A single memory mutation surfaced inline in the conversation. */
export interface MemoryUpdateEntry {
  action: "write" | "update" | "delete" | "summarize" | "cleanup" | "snapshot";
  memoryType: "fact" | "style" | "summary" | "tool" | "snapshot";
  key: string;
  summary: string;
  count?: number;
  timestamp: string;
}

/**
 * Collect memory mutations (write / update / delete / summarize /
 * cleanup) from the `execution_timeline` so MessageBubble can render
 * them as inline "记忆" rows. De-dupe by (action, key) to keep the
 * list stable across re-renders / reload.
 */
export function extractMemoryUpdates(events?: ExecutionTimelineEvent[]): MemoryUpdateEntry[] {
  if (!events) return [];
  const out: MemoryUpdateEntry[] = [];
  const seen = new Set<string>();
  for (const ev of events) {
    if (ev.type !== "memory_update") continue;
    const action = (ev.memory_action ?? "write") as MemoryUpdateEntry["action"];
    const memoryType = (ev.memory_type ?? "fact") as MemoryUpdateEntry["memoryType"];
    const key = ev.memory_key ?? "";
    const summary = ev.memory_summary ?? "";
    if (!summary && !key) continue;
    const dedupeKey = `${action}::${key}`;
    if (seen.has(dedupeKey)) continue;
    seen.add(dedupeKey);
    out.push({
      action,
      memoryType,
      key,
      summary,
      count: ev.memory_count,
      timestamp: ev.created_at,
    });
  }
  return out;
}

/**
 * Memory-recall event fired when the agent
 * re-pulled related facts this turn. Distinct from
 * `MemoryUpdateEntry` (which fires on writes). Used to render the
 * inline "💭 想到 N 条" chip.
 */
export interface MemoryRecallEntry {
  trigger: "first_turn" | "interval" | "drift";
  count: number;
  totalChars?: number;
  previewKeys: string[];
  turnNumber: number;
  timestamp: string;
}

/**
 * Collect memory-recall events. We don't dedupe as aggressively
 * as `extractMemoryUpdates` because two recalls on adjacent turns
 * carry different `turnNumber`s and should both render.
 */
export function extractMemoryRecalls(events?: ExecutionTimelineEvent[]): MemoryRecallEntry[] {
  if (!events) return [];
  const out: MemoryRecallEntry[] = [];
  const seen = new Set<string>();
  for (const ev of events) {
    if (ev.type !== "memory_recall") continue;
    const count = ev.memory_recall_count ?? 0;
    if (count === 0) continue;
    const turnNumber = ev.memory_recall_turn_number ?? 0;
    const trigger = (ev.memory_recall_trigger ?? "interval") as MemoryRecallEntry["trigger"];
    const dedupeKey = `${trigger}::${turnNumber}`;
    if (seen.has(dedupeKey)) continue;
    seen.add(dedupeKey);
    out.push({
      trigger,
      count,
      totalChars: ev.memory_recall_total_chars,
      previewKeys: ev.memory_recall_preview_keys ?? [],
      turnNumber,
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
  // render as a SECOND card — the duplicate "工具结果出现两次" symptom. We
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
  // i.e. two cards per tool call, producing "一件事三行" (one tool
  // shown as three rows).
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
          // event carried an error payload, "skipped" if the tool
          // never actually ran (approval timeout / user denial) —
          // see `groupTimeline` and `utils/toolOutcome.ts`.
          // Previously this line collapsed everything-but-running
          // to "done", which is why reloads painted a green
          // "运行中" badge on tools that had clearly errored
          // (terminal exit ≠ 0) and "已完成" on ones that never
          // ran at all.
          status: g.status,
          // Stash a one-line reason on the meta so the renderer
          // can show "已中断" / "失败" / "已跳过" instead of the
          // generic "执行中" pill. The renderer reads this via
          // `meta.errorReason`; older versions of the renderer
          // will simply ignore the field.
          errorReason:
            g.statusNote?.startsWith("执行已中断")
              ? g.statusNote
              : g.status === "error" || g.status === "skipped"
                ? (g.statusNote ?? (g.status === "skipped" ? "未执行" : "执行失败"))
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

  // When content has body text, show reasoning as a reasoning
  // block (folded) + body as text (normal case).
  // When content is empty but reasoning has text, this typically
  // means one of two things:
  //    routed ALL text to reasoning_content.
  //    swapped content → reasoning_content at flush time.
  // In both cases, the text IS reasoning — it was in
  // `reasoning_content` on the backend. Show it as a reasoning
  // step (folded inside ProcessTimeline), NOT as body text.
  if (hasBody) {
    if (reasoning) {
      steps.push({ kind: "reasoning", content: reasoning });
    }
    if (toolEvents.length > 0) {
      steps.push({ kind: "tools", events: toolEvents });
    }
    steps.push({ kind: "text", content: parsed.body });
  } else if (reasoning) {
    // Content is empty, reasoning has text. This can mean:
    //    routed ALL text to reasoning_content.
    //    swapped content → reasoning_content at flush time.
    //    split reasoning into thinking + visible reply, but the
    //    persisted message still has empty content (edge case).
    // Try to split the reasoning into thinking + visible body.
    // This handles the MiniMax M3 case where an unclosed <arg_key>tag
    // causes both thinking and the visible reply to end up in
    // reasoning_content, and the backend split may not have been
    // applied (e.g. persisted messages from before the fix).
    const splitResult = splitUnclosedThinkReasoning(reasoning);
    if (splitResult) {
      if (splitResult.reasoning) {
        steps.push({ kind: "reasoning", content: splitResult.reasoning });
      }
      if (toolEvents.length > 0) {
        steps.push({ kind: "tools", events: toolEvents });
      }
      steps.push({ kind: "text", content: splitResult.body });
    } else {
      // No split point found — entire reasoning is actual thinking.
      // Show as reasoning step (folded inside ProcessTimeline).
      if (toolEvents.length > 0) {
        steps.push({ kind: "tools", events: toolEvents });
      }
      steps.push({ kind: "reasoning", content: reasoning });
    }
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
 * Adjacent `kind: "reasoning"` steps are merged into
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
  return stampReasoningTimings(mergeAdjacentReasoning(raw));
}

/** 一个 step 里第一个/最后一个真实时间戳（只认工具事件的 `created_at`）。 */
function stepTimeBounds(
  step: TimelineStep,
): { first?: string; last?: string } {
  if (step.kind === "reasoning") {
    return { first: step.startedAt, last: step.endedAt };
  }
  if (step.kind !== "tools") return {};
  let first: string | undefined;
  let last: string | undefined;
  for (const e of step.events) {
    if (!e.created_at) continue;
    if (first === undefined) first = e.created_at;
    last = e.created_at;
  }
  return { first, last };
}

/**
 * 给恢复态的思考段补时间戳 —— 从**相邻工具事件的真实 `created_at`** 反推。
 *
 * 2026-10-01：用户反馈重载历史后「思考」行不显示耗时。原因是流式时
 * `appendReasoningStep` 打的 `startedAt/endedAt` 只活在前端内存里，
 * 服务端持久化的 `reasoning_content` 是一整段字符串，**没有分段时间**。
 *
 * 但 `execution_timeline` 的事件带真实 `created_at`，而思考段恰好夹在
 * 工具调用之间 —— 于是「这段思考从上一批工具结束到下一批工具开始有多久」
 * 是可以从真实数据算出来的，不是编的。
 *
 * 覆盖不到的情况（诚实地不显示，而不是显示错的）：
 * - **第一段**思考：它的开始是本轮起点，恢复态拿不到（`duration_ms`
 *   不持久化），所以只有 endedAt，没有耗时。
 * - 末尾那段思考：后面没有工具调用可作结束锚点，同样无耗时。
 *
 * 即「有真实边界的段才显示时间」。这比统一显示 0 秒诚实，也比一律不显示
 * 多给了一半段落的真实信息。
 */
function stampReasoningTimings(steps: TimelineStep[]): TimelineStep[] {
  if (steps.length === 0) return steps;
  const bounds = steps.map(stepTimeBounds);
  return steps.map((step, i) => {
    if (step.kind !== "reasoning") return step;
    if (step.startedAt) return step; // 流式时已打戳，不覆盖
    // 结束锚点：本段之后第一个带时间戳的 step 的起点
    let endedAt: string | undefined;
    for (let j = i + 1; j < steps.length; j++) {
      if (bounds[j].first) {
        endedAt = bounds[j].first;
        break;
      }
    }
    // 开始锚点：之前最后一个带时间戳的 step 的终点
    let startedAt: string | undefined;
    for (let j = i - 1; j >= 0; j--) {
      if (bounds[j].last) {
        startedAt = bounds[j].last;
        break;
      }
    }
    if (!endedAt || !startedAt) return step;
    return { ...step, startedAt, endedAt };
  });
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
  // 合并相邻思考时保住时间跨度：取最早的开始、最晚的结束。否则
  // 「思考 · 持续了 N 秒」会算成 0 秒（只拿到后一段的 endedAt）。
  let bufStart: string | undefined;
  let bufEnd: string | undefined;
  const flush = () => {
    if (buf !== null) {
      out.push({ kind: "reasoning", content: buf, startedAt: bufStart, endedAt: bufEnd });
      buf = null;
      bufStart = undefined;
      bufEnd = undefined;
    }
  };
  for (const step of steps) {
    if (step.kind === "reasoning") {
      buf = buf === null ? step.content : buf + "\n\n" + step.content;
      if (bufStart === undefined) bufStart = step.startedAt;
      if (step.endedAt) bufEnd = step.endedAt;
      continue;
    }
    if (step.kind === "text" && isThinkBlockResidue(step.content)) {
      // Drop the residue silently — it gets folded into the surrounding
      // reasoning buffer at flush time. This is the key fix for
      // "思考被切成多段" when the LLM outputs multiple adjacent think
      // blocks with short markdown headings between them.
      continue;
    }
    flush();
    out.push(step);
  }
  flush();
  return out;
}

/**
 * Attempt to split a reasoning_content blob that actually contains both
 * the model's thinking and its visible reply. This happens when MiniMax M3
 * opens a ‌​ tag but never emits the closing ‌​,
 * causing ThinkBlockParser to route the entire remainder (thinking +
 * visible reply) into reasoning_content.
 *
 * MiniMax M3's actual output pattern:
 *   ‌​\n思考过程...\n\n正文回复
 * The thinking and visible reply are separated by a double newline.
 * We detect this boundary and split the blob.
 *
 * Returns `{reasoning, body}` if a split point was found, or `null` if
 * the entire blob appears to be reasoning.
 */
function splitUnclosedThinkReasoning(reasoningContent: string): { reasoning: string; body: string } | null {
  const trimmed = reasoningContent.trim();
  if (!trimmed) return null;

  // Look for the first significant paragraph break (double newline)
  // that could mark the boundary between thinking and the visible reply.
  // Skip boundaries that are very early (first 20 chars) — those are
  // likely just formatting within the thinking block.
  const minReasoningLen = 20;
  let searchStart = 0;

  while (searchStart < trimmed.length) {
    const remaining = trimmed.slice(searchStart);
    const pos = remaining.indexOf("\n\n");
    if (pos === -1) break;

    const absPos = searchStart + pos;
    if (absPos < minReasoningLen) {
      searchStart = absPos + 2;
      continue;
    }

    const candidateReasoning = trimmed.slice(0, absPos);
    const candidateContent = trimmed.slice(absPos).trim();

    // If the content after the boundary is empty, skip
    if (!candidateContent) {
      searchStart = absPos + 2;
      continue;
    }

    // Check if the content after the boundary looks like a user-facing
    // reply rather than more reasoning.
    if (!looksLikeUntaggedReasoningLocal(candidateContent)) {
      return { reasoning: candidateReasoning, body: candidateContent };
    }

    // Content after this boundary still looks like reasoning —
    // continue searching for a later boundary
    searchStart = absPos + 2;
  }

  return null;
}

/**
 * Local version of the untagged reasoning detection for use in
 * splitUnclosedThinkReasoning. Uses tight signals to avoid false positives.
 */
function looksLikeUntaggedReasoningLocal(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed.length < 40) return false;
  const head = trimmed.slice(0, 360).toLowerCase();
  const signals = [
    "trying to", "i think", "i see", "probably", "could mean",
    "might be", "reconstruct", "let me trace", "let me check",
    "actually,", "seems like", "looking at", "the diff shows",
    "the issue is", "i believe", "i suspect", "i'm trying", "i am trying",
    "让我来", "我需要先", "我先看看",
    "我先分析", "我直接看", "我想想",
    "我认为应该", "我觉得可能",
    "看起来像是", "实际上可能",
  ];
  return signals.some((s) => head.includes(s));
}

/**
 * Detect the "LLM-emitted headings between adjacent think blocks"
 * pattern. Examples that should return true:
 *   - `**Clarifying directory paths**`
 *   - `### Identifying file structure`
 *   - stray ‌​ tags (in case any leak through)
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
