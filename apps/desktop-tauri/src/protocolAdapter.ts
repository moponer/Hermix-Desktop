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
  const reasoningRaw = (wire as Record<string, unknown>).reasoningContent;
  const reasoningContent =
    typeof reasoningRaw === "string" ? reasoningRaw : undefined;
  return {
    id,
    role,
    content: wire.content,
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
 * Whole-array adapter: project an entire wire response into a UI
 * array in one call. Returns rows the renderer cares about with
 * the directive strip already applied.
 */
export function wireArrayToChatMessages(
  wires: ReadonlyArray<WireMessage>,
  sessionId: string,
  detectToolError: (content: string) => string | null,
): ChatMessage[] {
  const out: ChatMessage[] = [];
  wires.forEach((wire, i) => {
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
