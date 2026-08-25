import type { ChatMessage, TimelineStep, ExecutionTimelineEvent } from "../types";
import type { CompressMethod } from "../utils/imageCompressor";

// pendingImages 数据结构。
// - file: 浏览器 File 句柄（几 KB），不存 base64（8MB+）
// - blobUrl: 浏览器内部引用，src= 直接用，零拷贝
// - isCompressed / method: UI 角标提示压缩状态
export interface PendingImage {
  file: File;
  blobUrl: string;
  sizeBytes: number;
  mimeType: string;
  isCompressed: boolean;
  method: CompressMethod;
  name: string;
}

/**
 * Remove text steps that verbatim duplicate a reasoning step.
 *
 * When the backend emits untagged reasoning (or an unclosed `<think>`
 * tag), the same text can land in both `reasoningContent` (rendered
 * inside the collapsed thinking block) and `content` (rendered as body
 * text). If the streaming retro-conversion in App.tsx fails to strip it
 * from `content`, the body shows twice. This is a render-time safety
 * net: any text step whose trimmed content equals a reasoning step's
 * trimmed content is dropped, so the body is never a copy of the
 * thinking block. Genuine answers (body ≠ thinking) pass through
 * untouched.
 */
function dedupeTextSteps(steps: TimelineStep[] | undefined): TimelineStep[] | undefined {
  if (!steps || steps.length === 0) return steps;
  const reasoningTrimmed = new Set(
    steps
      .filter((s) => s.kind === "reasoning")
      .map((s) => (s as { content?: string }).content?.trim() ?? ""),
  );
  const deduped = steps.filter((s) => {
    if (s.kind !== "text") return true;
    const t = (s as { content?: string }).content?.trim() ?? "";
    if (t.length === 0) return false; // drop empty text steps too
    return !reasoningTrimmed.has(t);
  });
  return deduped.length > 0 ? deduped : undefined;
}

/**
 * Merge consecutive assistant messages into a single rendered bubble.
 *
 * Background: the backend `run_stream` may produce multiple Assistant
 * entries per user turn (one per LLM call within the same turn).  When
 * rendered as-is the chat shows N stacked empty/partial bubbles for one
 * user message.  This function collapses a run of adjacent Assistant
 * messages into a single bubble whose body concatenates the text in
 * order and whose tool timeline concatenates in order.  Empty
 * placeholders (no content and no tool calls) are dropped entirely.
 *
 * Non-assistant messages (User / Approval / Clarify / Status) are never
 * merged and are returned as-is, in original order, separated by the
 * merged assistant runs.
 */
export function mergeConsecutiveAssistantMessages(messages: ChatMessage[]): ChatMessage[] {
  const out: ChatMessage[] = [];
  let i = 0;
  while (i < messages.length) {
    const m = messages[i];
    if (m.role !== "assistant") {
      out.push(m);
      i += 1;
      continue;
    }
    // Collect the run of consecutive assistant messages.
    let j = i;
    const run: ChatMessage[] = [];
    while (j < messages.length && messages[j].role === "assistant") {
      run.push(messages[j]);
      j += 1;
    }
    // Filter out empty placeholders.
    const nonEmpty = run.filter(
      (r) =>
        (r.content && r.content.trim().length > 0) ||
        (r.tool_calls && r.tool_calls.length > 0) ||
        (r.execution_timeline && r.execution_timeline.length > 0) ||
        (r.reasoningContent && r.reasoningContent.trim().length > 0) ||
        // Empty assistant placeholders can still hold meaningful unified
        // steps (e.g. a pending approval_request sitting mid-stream before
        // any final text lands). Dropping the whole row here hides the
        // approval card. Mirror isRowRenderable's hasUnifiedSteps rule.
        (Array.isArray(r.unified_steps) && r.unified_steps.length > 0)
    );
    if (nonEmpty.length === 0) {
      // Whole run is empty placeholders → drop entirely.
      i = j;
      continue;
    }
    if (nonEmpty.length === 1) {
      const single = nonEmpty[0];
      const singleSteps: TimelineStep[] = [];
      const reasoning = (single.reasoningContent || "").replace(/<\/?think>/gi, "").trim();
      const toolEvents = (single.execution_timeline || []).filter(
        (e) => e.type === "tool_start" || e.type === "tool_stdout" || e.type === "tool_complete",
      );
      const hasBody = !!(single.content && single.content.trim());
      if (hasBody) {
        if (reasoning) {
          singleSteps.push({ kind: "reasoning", content: reasoning });
        }
        if (toolEvents.length > 0) {
          singleSteps.push({ kind: "tools", events: toolEvents });
        }
        singleSteps.push({ kind: "text", content: single.content });
      } else if (reasoning) {
        if (toolEvents.length > 0) {
          singleSteps.push({ kind: "tools", events: toolEvents });
        }
        singleSteps.push({ kind: "reasoning", content: reasoning });
      } else if (toolEvents.length > 0) {
        singleSteps.push({ kind: "tools", events: toolEvents });
      }
      // unified_steps is the authoritative real-time pipeline (streaming
      // chunks + onApprovalRequest write into it). Rebuilding steps from
      // legacy fields (reasoningContent/execution_timeline) silently drops
      // any approval_request step — the user's active waiting surface.
      // When the message already carries non-legacy steps, trust them.
      const hasSpecialStep = (single.unified_steps ?? []).some(
        (x) => x.kind !== "reasoning" && x.kind !== "tools" && x.kind !== "text",
      );
      out.push({
        ...single,
        unified_steps: dedupeTextSteps(
          hasSpecialStep
            ? single.unified_steps
            : singleSteps.length > 0
              ? singleSteps
              : single.unified_steps,
        ),
      });
      i = j;
      continue;
    }
    // Concatenate contents, execution_timeline, tool_calls, and reasoningContent.
    const mergedContent = nonEmpty
      .map((r) => r.content || "")
      .filter((c) => c.length > 0)
      .join("\n\n");
    const mergedTimeline = nonEmpty.flatMap(
      (r) => r.execution_timeline || []
    );
    const mergedToolCalls = nonEmpty.flatMap(
      (r) => r.tool_calls || []
    );
    const mergedReasoning = nonEmpty
      .map((r) => r.reasoningContent || "")
      .filter((c) => c.length > 0)
      .join("\n\n");
    const unifiedSteps: TimelineStep[] = [];
    for (let ri = 0; ri < nonEmpty.length; ri++) {
      const r = nonEmpty[ri];
      const isLastRow = ri === nonEmpty.length - 1;
      const reasoning = (r.reasoningContent || "").replace(/<\/?think>/gi, "").trim();
      const toolEvents = (r.execution_timeline || []).filter(
        (e) => e.type === "tool_start" || e.type === "tool_stdout" || e.type === "tool_complete",
      );
      const hasBody = !!(r.content && r.content.trim());
      if (hasBody) {
        if (reasoning) {
          unifiedSteps.push({ kind: "reasoning", content: reasoning });
        }
        if (toolEvents.length > 0) {
          unifiedSteps.push({ kind: "tools", events: toolEvents });
        }
        unifiedSteps.push({ kind: "text", content: r.content });
      } else if (isLastRow && reasoning) {
        if (toolEvents.length > 0) {
          unifiedSteps.push({ kind: "tools", events: toolEvents });
        }
        unifiedSteps.push({ kind: "reasoning", content: reasoning });
      } else {
        if (reasoning) {
          unifiedSteps.push({ kind: "reasoning", content: reasoning });
        }
        if (toolEvents.length > 0) {
          unifiedSteps.push({ kind: "tools", events: toolEvents });
        }
      }
    }
    const mergedStopReason = nonEmpty
      .map((r) => r.stopReason)
      .filter((s): s is string => !!s && s.length > 0)
      .pop();
    const last = nonEmpty[nonEmpty.length - 1];
    // Preserve any approval_request (and other non-legacy) steps carried
    // by the original rows — reconstructing from legacy fields alone would
    // drop the pending approval card. Rebuilt steps win for the rest.
    const carriedSteps = nonEmpty.flatMap((r) => r.unified_steps ?? []).filter(
      (x) => x.kind !== "reasoning" && x.kind !== "tools" && x.kind !== "text",
    );
    out.push({
      ...last,
      content: mergedContent,
      tool_calls:
        mergedToolCalls.length > 0 ? mergedToolCalls : last.tool_calls,
      execution_timeline:
        mergedTimeline.length > 0
          ? mergedTimeline
          : last.execution_timeline,
      reasoningContent:
        mergedReasoning.length > 0
          ? mergedReasoning
          : last.reasoningContent,
      unified_steps: dedupeTextSteps(
        carriedSteps.length > 0
          ? [...(unifiedSteps.length > 0 ? unifiedSteps : []), ...carriedSteps]
          : unifiedSteps.length > 0
            ? unifiedSteps
            : undefined,
      ),
      stopReason: mergedStopReason ?? last.stopReason,
    });
    i = j;
  }
  return out;
}

/**
 * Should the assistant bubble at `idx` render its avatar + meta line?
 *
 * Rule: the avatar + meta line render only on the first assistant message
 * of an agent loop (the one immediately after a user message). A merged
 * assistant group collapses the loop's first message into a placeholder,
 * so when locating the loop start we skip *consecutive* assistant messages
 * (not just tools) until the first non-{tool, assistant} message, then check
 * whether it is a user message.
 */
export function shouldShowAssistantHeader(idx: number, messages: ReadonlyArray<{ role?: ChatMessage["role"] }>): boolean {
  const msg = messages[idx];
  if (!msg || msg.role !== "assistant") return false;
  let prevIdx = idx - 1;
  while (prevIdx >= 0) {
    const r = messages[prevIdx]?.role;
    if (r === undefined) {
      prevIdx--;
      continue;
    }
    if (r === "assistant" || r === "tool" || r === "approval" || r === "clarify") {
      prevIdx--;
    } else {
      break;
    }
  }
  const effectivePrev = prevIdx >= 0 ? messages[prevIdx] : null;
  return effectivePrev === null || effectivePrev.role === "user";
}

/**
 * Merge legacy `role: "tool"` messages into the preceding assistant's
 * `execution_timeline`. This handles sessions created by older codepaths
 * that emitted separate tool rows — the frontend now expects all tool
 * events to live inside the assistant message's timeline.
 */
export function mergeLegacyToolMessages(messages: ChatMessage[]): ChatMessage[] {
  const out: ChatMessage[] = [];
  for (const m of messages) {
    if (m.role !== "tool" || !m.tool) {
      out.push(m);
      continue;
    }
    // Merge into last assistant message
    const lastAssistant = [...out].reverse().find((msg) => msg.role === "assistant");
    if (!lastAssistant) {
      // No preceding assistant — keep as-is (shouldn't happen)
      out.push(m);
      continue;
    }
    // Replace the last assistant with an enriched copy
    out.pop();
    const timeline: ExecutionTimelineEvent[] = [...(lastAssistant.execution_timeline ?? [])];
    // Map tool meta to timeline events
    const tc = m.tool;
    const callId = tc.toolCallId ?? `legacy-${m.timestamp}`;
    // Add tool_start if not present
    const hasStart = timeline.some((e) => e.type === "tool_start" && e.tool_call_id === callId);
    if (!hasStart) {
      timeline.push({
        type: "tool_start",
        tool: tc.tool,
        arguments: tc.args as string | undefined,
        tool_call_id: callId,
        created_at: m.timestamp,
      });
    }
    // Add tool_complete if not present
    const hasComplete = timeline.some((e) => e.type === "tool_complete" && e.tool_call_id === callId);
    if (!hasComplete) {
      timeline.push({
        type: "tool_complete",
        tool: tc.tool,
        content: tc.output ?? "",
        tool_call_id: callId,
        created_at: m.timestamp,
      });
    }
    out.push({
      ...lastAssistant,
      execution_timeline: timeline,
    });
  }
  return out;
}

/**
 * Defensive filter for empty placeholders.
 *
 * Background: `handleSendMessage` optimistically appends a
 * `content: ""` assistant placeholder to the session so chunks can
 * land on it (keyed by `assistantId`). If the stream gets cancelled
 * before any chunk arrives, or if the WS drops mid-flight, that
 * placeholder stays in the array.
 */
export function isRowRenderable(m: ChatMessage): boolean {
  if (m.role === "user") {
    return (
      (typeof m.content === "string" && m.content.trim().length > 0) ||
      (Array.isArray(m.images) && m.images.length > 0)
    );
  }
  if (m.role !== "assistant") return true;
  if (m.stopReason && m.stopReason.length > 0) return true;
  const hasContent =
    typeof m.content === "string" && m.content.trim().length > 0;
  const hasToolCalls =
    Array.isArray(m.tool_calls) && m.tool_calls.length > 0;
  const hasTimeline =
    Array.isArray(m.execution_timeline) && m.execution_timeline.length > 0;
  const hasReasoning =
    typeof m.reasoningContent === "string" && m.reasoningContent.trim().length > 0;
  // Unified timeline may carry an inline `kind: "approval_request"`
  // step while the message body is still empty (server blocked waiting
  // for the user's allow/deny decision; LLM has not produced final
  // text). Without this the row gets filtered out — the approval card
  // would render nowhere even though its step is in `unified_steps`.
  const hasUnifiedSteps =
    Array.isArray(m.unified_steps) && m.unified_steps.length > 0;
  return hasContent || hasToolCalls || hasTimeline || hasReasoning || hasUnifiedSteps;
}
