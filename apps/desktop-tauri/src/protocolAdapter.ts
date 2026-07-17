// N-09 (2026-07-05): wire-→ in-memory chat row adapter.
//
// Lives between `GET /v1/sessions/{id}/messages` (snake_case + raw `extra`
// blob) and `ChatMessage` (typed `approval` / `tool` / `clarify` projections
// + UI-only fields). Adding a new approval/clarify/tool field is now a
// one-file change here, instead of an Edit-spread across `api.ts` and
// `types.ts` with a hand-written per-role projection in `getSessionMessages`.
//
// This module also owns the content-filter pipeline that strips internal
// system directives (`<system-reminder>`, `<plan>`) before the renderer
// sees them, so a leaked directive shows up as a missing substitution in
// the builder rather than a chat row full of `<system-reminder>…</system-reminder>`.

import type { ChatMessage, ToolMessageMeta, ApprovalMessageMeta } from "./types";

// The shape the server actually returns. Snake_case; optional id /
// timestamp; raw `extra` for approvals and `tool_name` for tools.
// Exported as `WireMessage` so api.ts can accept raw server JSON
// without an `as unknown as ChatMessage` cast.
export type WireMessage = {
  id?: string;
  role: string;
  content: string;
  timestamp?: string;
  // v33 (2026-07-06): `created_at` is what the backend actually
  // persists (the SQLite `messages.created_at` column). The previous
  // code only read `wire.timestamp`, which was always `undefined`,
  // falling back to `new Date().toISOString()` — producing "刚刚" /
  // "1 分钟前" on every reload regardless of how old the turn
  // actually was. Read `created_at` first when present.
  created_at?: string;
  model?: string;
  stopReason?: string;
  execution_backend?: "local" | "sandbox";
  tool_calls?: Array<{
    name?: string;
    function?: { name?: string; arguments?: string };
    arguments?: string;
    status?: "running" | "done" | "error";
    output?: string;
  }>;
  tool_name?: string;
  tool_arguments?: string;
  execution_timeline?: ChatMessage["execution_timeline"];
  extra?: {
    request_id?: string;
    tool?: string;
    arguments?: unknown;
    risk?: string;
    decision?: "approved" | "denied" | null;
    decided_at?: string | null;
    expires_at?: number | null;
    linked_assistant_id?: string | null;
  };
  [k: string]: unknown;
};

// Non-greedy so nested or multi-tag blocks each drop independently.
const SYSTEM_DIRECTIVES_REGEX = /<system-reminder>[\s\S]*?<\/system-reminder>|<plan>[\s\S]*?<\/plan>/gi;

/**
 * Strip internal `<system-reminder>` / `<plan>` blocks that the LLM
 * sometimes echoes back from its prompt. Pure; exposed for tests.
 */
export function stripSystemDirectives(content: string): string {
  return content.replace(SYSTEM_DIRECTIVES_REGEX, "").trim();
}

/**
 * Project a `tool_calls` wire entry (OpenAI-style: `name` may live at
 * the top level OR under `function`, per `ToolCall::flatten` on the
 * server) into the legacy chat shape `{ name, status, output? }`.
 *
 * `status` is forced to `"done"` for every reloaded call — the
 * historical `status: "running"` pill bug appeared here because the
 * legacy StatusPill renders `status === undefined` as "运行中".
 */
function projectToolCalls(
  wire: WireMessage["tool_calls"],
): ChatMessage["tool_calls"] {
  if (!wire || wire.length === 0) return undefined;
  const projected = wire.map((tc) => ({
    name:
      tc.name ??
      tc.function?.name ??
      "tool",
    status: "done" as const,
    output: tc.arguments ?? tc.function?.arguments ?? tc.output,
  }));
  return projected.length > 0 ? projected : undefined;
}

/**
 * Build the typed `tool` meta for a `role: "tool"` row.
 *
 * `errorReason` extraction reuses `detectToolError` so the reload
 * path and the live-stream path agree on which tool outputs count
 * as errors (otherwise a `shell exit 1` row would render as
 * "completed" on reload but red on a fresh stream — see v5 in
 * `getSessionMessages`).
 */
function buildToolMeta(
  wire: WireMessage,
  detectToolError: (content: string) => string | null,
): ToolMessageMeta {
  const startedAt = wire.timestamp ?? new Date().toISOString();
  return {
    tool: wire.tool_name ?? "tool",
    args: wire.tool_arguments,
    status: "done",
    output: wire.content,
    errorReason: detectToolError(wire.content) ?? undefined,
    startedAt,
    completedAt: startedAt,
  };
}

/**
 * Build the typed `approval` meta for a `role: "approval"` row. Maps
 * the snake_case `extra` blob (mirrors `ApprovalRecord` 1:1) into the
 * camelCase shape that `InlineApprovalCard` expects.
 */
function buildApprovalMeta(wire: WireMessage): ApprovalMessageMeta {
  const ex = wire.extra ?? {};
  return {
    requestId: ex.request_id ?? "",
    tool: ex.tool ?? "tool",
    args: ex.arguments,
    riskLevel: ex.risk,
    decision: ex.decision ?? null,
    decidedAt: ex.decided_at ?? null,
    expiresAt: typeof ex.expires_at === "number" ? ex.expires_at : null,
    linkedAssistantId: ex.linked_assistant_id ?? null,
  };
}

/**
 * Filter the rows the chat UI cares about. Drops server-side `system`
 * rows to avoid leaking the full reconstructed prompt (~23K chars of
 * AGENTS.md + skills) and drops `clarify` rows that may have been
 * echoed back during a fresh session resume (which would otherwise
 * cause a duplicated `InlineClarifyCard`).
 *
 * @param wire         a single wire-format message row
 * @param rowIndex     the row's ordinal in the wire response, used to
 *                     generate a deterministic fallback id
 * @param sessionId    the session the row belongs to (fallback id prefix)
 */
export function wireToChatMessage(
  wire: WireMessage,
  rowIndex: number,
  sessionId: string,
  detectToolError: (content: string) => string | null,
): ChatMessage | null {
  // Drop rows the renderer should never see.
  if (
    wire.role !== "user" &&
    wire.role !== "assistant" &&
    wire.role !== "approval" &&
    wire.role !== "tool"
  ) {
    return null;
  }
  const id = wire.id ?? `${sessionId}-${wire.role}-${rowIndex}`;
  // v33 (2026-07-06): prefer `created_at` (the SQLite-persisted
  // wall-clock) over the optional legacy `timestamp` field. Fall
  // back to `new Date()` only as a last resort — pre-existing
  // sessions written before the column was exposed would land
  // here, but they should migrate on the next save.
  const timestamp =
    wire.created_at ?? wire.timestamp ?? new Date().toISOString();
  const role = wire.role as ChatMessage["role"];
  // tool rows: rebuild a `ToolMessageMeta` from the server payload.
  // `MessageRole::Tool` rows persist `content` (the tool output)
  // and `tool_name` (the original `Message::name`); the server also
  // reconstructs `tool_arguments` from the linked assistant row's
  // `tool_calls[].function.arguments` at serve time so a reloaded
  // ToolMessageCard shows the verb + primary-arg preview instead of
  // "调用工具".
  if (wire.role === "tool") {
    return {
      id,
      role: "tool",
      content: wire.content,
      timestamp,
      tool: buildToolMeta(wire, detectToolError),
    };
  }
  // approval rows: project the `extra` blob into a typed meta.
  if (wire.role === "approval") {
    return {
      id,
      role: "approval",
      content: "",
      timestamp,
      approval: buildApprovalMeta(wire),
    };
  }
  // user / assistant: forward every field the server persists so a
  // session reload shows tool timelines, interrupted chips, and
  // execution-backend badges that were visible mid-stream.
  const projectedToolCalls = projectToolCalls(wire.tool_calls);
  // `reasoningContent` was originally a non-typed sneaking-through
  // read; type it properly now.
  //
  // 2026-07-08 (user, 思考折叠在输出结束后消失):
  // the server's `ProtocolMessage` field is `reasoning_content`
  // (snake_case; no `rename_all = "camelCase"` on the struct), so the
  // wire payload's key is `reasoning_content`. The previous code
  // read `wire.reasoningContent` (camelCase) which is always
  // `undefined` on this path. `onDone` then re-fetches the messages
  // 500ms later (App.tsx::onDone) and replaces the in-memory message
  // — which DOES have `reasoningContent` from `onReasoningChunk` —
  // with this `undefined` projection. Net effect: the folding block
  // is visible during the stream (read from local in-memory state)
  // and disappears once the post-stream re-fetch lands.
  //
  // Read both casings for forward-compat: if we ever add
  // `#[serde(rename_all = "camelCase")]` to `ProtocolMessage`,
  // the camelCase branch will start returning truthy values and
  // we'd drop into the snake_case branch via `??`.
  const wireRecord = wire as Record<string, unknown>;
  const reasoningRaw =
    wireRecord.reasoning_content ?? wireRecord.reasoningContent;
  let reasoningContent =
    typeof reasoningRaw === "string" ? reasoningRaw : undefined;
  let visibleContent = wire.content;
  // 2026-07-17: defence for messages already persisted before the backend
  // fix. If the visible content is an unwrapped reasoning monologue (the
  // model ignored the system prompt and emitted "I think..." / "trying
  // to..." without a ` <think` tag), move it to reasoningContent so the UI
  // folds it instead of leaking it into the chat body.
  if (
    visibleContent &&
    !visibleContent.includes("<think") &&
    !visibleContent.includes("</think") &&
    looksLikeUntaggedReasoning(visibleContent)
  ) {
    reasoningContent = reasoningContent
      ? reasoningContent + "\n\n" + visibleContent
      : visibleContent;
    visibleContent = "";
  }
  return {
    id,
    role,
    content: visibleContent,
    timestamp,
    model: wire.model,
    stopReason: wire.stopReason,
    execution_backend: wire.execution_backend,
    tool_calls: projectedToolCalls,
    execution_timeline: wire.execution_timeline,
    reasoningContent,
    // Carry the raw `extra` / tool-name fields through so any code
    // path that hasn't been migrated to the typed projection still
    // sees the server's data.
    tool_name: wire.tool_name,
    tool_arguments: wire.tool_arguments,
    extra: wire.extra,
  };
}

/**
 * Conservative heuristic: detect an unwrapped reasoning monologue that
 * the model forgot to put inside `  <think...`. Targets the common
 * failure mode where the assistant starts with self-directed analysis
 * ("I think...", "trying to reconstruct...", "probably...") instead of
 * a direct user-facing reply.
 */
function looksLikeUntaggedReasoning(text: string): boolean {
  const trimmed = text.trim();
  if (trimmed.length < 40) return false;
  const head = trimmed.slice(0, 240).toLowerCase();
  const signals = [
    "trying to",
    "i think",
    "i see",
    "probably",
    "could mean",
    "might be",
    "reconstruct",
    "let me trace",
    "let me check",
    "actually,",
    "seems like",
    "looking at",
    "the diff shows",
    "the issue is",
    "i believe",
    "i suspect",
    "i'm trying",
    "i am trying",
  ];
  return signals.some((s) => head.includes(s));
}

/**
 * Whole-array adapter: project an entire wire response into a UI
 * array in one call. Returns rows the renderer cares about with
 * the directive strip already applied.
 *
 * 2026-07-10: Tool rows are now MERGED into the preceding assistant
 * message's `execution_timeline` instead of being emitted as separate
 * `role: "tool"` messages. During live streaming, tool events
 * (tool_start / tool_stdout / tool_complete) are appended to the
 * assistant's execution_timeline and rendered as a collapsible block
 * inside the assistant bubble. Without this merge, a session reload
 * would produce a different visual layout (tool as separate top-level
 * cards above the avatar vs. inline below the avatar).
 */
export function wireArrayToChatMessages(
  wires: ReadonlyArray<WireMessage>,
  sessionId: string,
  detectToolError: (content: string) => string | null,
): ChatMessage[] {
  const out: ChatMessage[] = [];
  wires.forEach((wire, i) => {
    // Tool rows: merge into the preceding assistant message's
    // execution_timeline instead of creating a standalone message.
    if (wire.role === "tool") {
      const ts = wire.created_at ?? wire.timestamp ?? new Date().toISOString();
      // Find the last assistant message in the output array.
      // The server preserves insertion order, so the tool row
      // always comes right after the assistant that called it.
      const lastAssistantIdx = (() => {
        for (let j = out.length - 1; j >= 0; j--) {
          if (out[j].role === "assistant") return j;
        }
        return -1;
      })();

      if (lastAssistantIdx >= 0) {
        const assistant = out[lastAssistantIdx];
        const existingTimeline = assistant.execution_timeline ?? [];
        // De-duplicate: the backend persists BOTH the assistant's
        // `execution_timeline` (from streaming tool_start/tool_stdout/
        // tool_complete events) AND separate `role: "tool"` rows.
        // If the assistant already has timeline events for this
        // tool_call_id, the streaming data is more detailed — skip
        // synthesizing duplicate events from the tool row.
        const toolCallId = wire.id ?? `${sessionId}-tool-${i}`;
        const alreadyHasTimeline = existingTimeline.some(
          (e) => e.tool_call_id === toolCallId,
        );
        if (!alreadyHasTimeline) {
          // Also check by tool name + args signature for older
          // sessions where tool_call_id wasn't persisted on the
          // tool row but was on the timeline events.
          const toolName = wire.tool_name ?? "tool";
          const toolArgs = wire.tool_arguments;
          const hasBySig = existingTimeline.some(
            (e) => e.type === "tool_start" && e.tool === toolName,
          );
          if (!hasBySig) {
            const timeline = [...existingTimeline];
            timeline.push({
              type: "tool_start",
              tool: toolName,
              arguments: toolArgs,
              tool_call_id: toolCallId,
              created_at: ts,
            });
            if (wire.content) {
              timeline.push({
                type: "tool_stdout",
                tool_call_id: toolCallId,
                content: wire.content,
                created_at: ts,
              });
            }
            timeline.push({
              type: "tool_complete",
              tool_call_id: toolCallId,
              content: wire.content ?? "",
              created_at: ts,
            });
            out[lastAssistantIdx] = {
              ...assistant,
              execution_timeline: timeline,
            };
          }
        }
      }
      // If there's no preceding assistant (orphan tool row), drop it.
      // This shouldn't happen in normal operation but guards against
      // corrupted session data producing a stray tool card.
      return;
    }

    const projected = wireToChatMessage(wire, i, sessionId, detectToolError);
    if (projected) {
      out.push({
        ...projected,
        content: stripSystemDirectives(projected.content),
      });
    }
  });
  return out;
}

export const __test_only__ = { stripSystemDirectives, SYSTEM_DIRECTIVES_REGEX };
