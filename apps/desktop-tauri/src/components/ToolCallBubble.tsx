// Tool-call related bubbles: tool-row cards, the unified/process timeline
// renderer, the live terminal, and the file-change / memory meta blocks.
// Extracted from MessageBubble.tsx.
import React, { useState, useEffect, useMemo, useRef } from "react";
import {
  ChevronDown, ChevronRight, Loader2, AlertCircle, AlertTriangle,
  CheckCircle2, Check, Copy, CheckCheck, FileText, FilePlus,
  ArrowRight, Edit3, Trash2, Brain, Database, Wrench, SearchX,
} from "lucide-react";
import { MarkdownBody } from "./MarkdownBody";
import type { ChatMessage, TimelineStep } from "../types";
import { ApprovalRequestView } from "./ApprovalBubble";
import { ApprovalBatchView } from "./ApprovalBatchBubble";
import {
  parseContent, groupTimeline,
  type FileChangeEntry, type MemoryUpdateEntry, type MemoryRecallEntry,
} from "./messageParser";
import {
  ScreenshotPreview, WindowListPreview, WindowAtPointPreview, UiTreePreview,
  UiFindPreview, UiClickPreview, UiSingleNamePreview,
  isPathArg, extractFirstPath, extractLineStats,
} from "./bubbles/ToolOutputPreviews";
import { parseMemoryResult, memoryActionLabel, memoryStatusColor } from "../utils/memoryToolResult";
import {
  formatDuration, toolIcon, toolVerb, toolPrimaryArg, toolResultPeek,
  toolKind, summarizeError, memoryArchiveChip, unwrapToolOutput,
} from "./bubbleUtils";
import { ThinkingDots, ReasoningBlock } from "./ThinkingBubble";
export function ToolGroupCard({
  group,
  onPathClick,
  awaitingApproval = false,
}: {
  group: import("./messageParser").ToolGroup;
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /** True when a sibling `kind: "approval_request"` step is still
   *  pending the user's decision. Tool status flips from "运行中"
   *  to "等待审批" so the user knows the agent is blocked on them,
   *  not stuck mid-execution. */
  awaitingApproval?: boolean;
}) {
  const isRunning = group.status === "running";
  const isError = group.status === "error";
  const [open, setOpen] = useState(false);

  // Memoize so large stdout outputs aren't re-joined every render —
  // a tool with 500 chunks would otherwise concatenate 500 strings on
  // each parent re-render for an identical result.
  const { startEv, output, hasOutput } = useMemo(() => {
    const startEv = group.events.find((e) => e.type === "tool_start");
    const completeEv = group.events.find((e) => e.type === "tool_complete");
    const stdout = group.events
      .filter((e) => e.type === "tool_stdout")
      .map((e) => e.content ?? "")
      .join("");
    const output = completeEv?.content ?? stdout ?? "";
    return { startEv, output, hasOutput: output.length > 0 };
  }, [group.events]);

  const toolName = group.tool;
  const args = startEv?.arguments;
  const Icon = toolIcon(toolName);
  const verb = toolVerb(toolName, args);
  const primary = isError
    ? (group.statusNote ?? summarizeError(output) ?? toolPrimaryArg(toolName, args, output))
    : toolPrimaryArg(toolName, args, output);
  const stats = extractLineStats(toolName, args);
  const isPathLike = isPathArg(toolName, args, output);
  const primaryPath = isPathLike ? (output || args) : null;

  const resultPeek = useMemo(() => {
    if (isError) {
      return group.statusNote
        ?? summarizeError(output)
        ?? (output ? toolResultPeek(output, args) : null)
        ?? "执行出错";
    }
    if (!output || output.trim().length === 0) return null;
    if (isRunning && !group.endedAt) return null;
    return toolResultPeek(output, args);
  }, [output, args, group.status, group.endedAt, group.statusNote, isError, isRunning]);

  const handlePrimaryClick = (e: React.MouseEvent) => {
    if (isPathLike && primaryPath && onPathClick) {
      const path = extractFirstPath(primaryPath);
      if (path) {
        e.stopPropagation();
        onPathClick(path, e);
        return;
      }
    }
    if (hasOutput) setOpen((v) => !v);
  };

  return (
    <div className="tw-msg h-enter chat-tool-call chat-tool-call--tool">
      <button
        type="button"
        onClick={hasOutput ? () => setOpen((v) => !v) : undefined}
        className="tw-msg-row"
        data-tool-kind={toolKind(toolName)}
        data-state={isError ? "err" : isRunning ? "run" : "ok"}
      >
        <span className="tw-msg-icon-tile">
          <Icon className="icon-sm tw-msg-icon-svg" />
        </span>
        <span className="tw-msg-verb">{verb}</span>
        {primary && (
          <span
            className={isPathLike ? "tw-msg-primary tw-msg-primary--link" : "tw-msg-primary"}
            title={isPathLike ? `点击打开 ${primary}` : primary}
            onClick={isPathLike ? handlePrimaryClick : undefined}
          >
            {primary}
          </span>
        )}
        {resultPeek && !isError && (
          <span className="tw-msg-result-peek">{resultPeek}</span>
        )}
        <span className="tw-msg-right chat-status-inline">
          {(stats.added != null || stats.removed != null) && (
            <span className="tw-msg-stats">
              {stats.added != null && stats.added > 0 && (
                <span className="tw-msg-stat tw-msg-stat--add">+{stats.added}</span>
              )}
              {stats.removed != null && stats.removed > 0 && (
                <span className="tw-msg-stat tw-msg-stat--del">−{stats.removed}</span>
              )}
            </span>
          )}
          <span className={`tw-msg-state tw-msg-state--${isError ? "err" : isRunning ? "run" : "done"}`}>
            {awaitingApproval && isRunning ? (
              <><AlertCircle className="icon-sm shrink-0" />等待审批</>
            ) : isRunning ? (
              <><Loader2 className="icon-sm animate-spin shrink-0" />运行中</>
            ) : isError ? (
              <><AlertCircle className="icon-sm shrink-0" />失败</>
            ) : (
              <><Check className="icon-sm shrink-0" />已完成</>
            )}
          </span>
          {hasOutput && (
            <span className="tw-msg-chev">{open ? <ChevronDown className="icon-sm" /> : <ChevronRight className="icon-sm" />}</span>
          )}
        </span>
      </button>
      {isError && resultPeek && primary !== resultPeek && (
        <div className="tw-msg-err-hint" onClick={() => setOpen(true)} title="点击展开完整输出">
          <AlertCircle size={10} className="shrink-0" />
          <span className="truncate">{resultPeek}</span>
          <ChevronRight className="icon-sm shrink-0 tw-msg-chev-err" />
        </div>
      )}
      {open && hasOutput && !isError && (
        <div className="tw-msg-body">
          <LiveTerminal content={output} running={isRunning && !group.endedAt} />
        </div>
      )}
    </div>
  );
}
export function ProcessTimeline({
  steps,
  isStreaming,
  keepOpen = false,
  hasRunningTool,
  durationMs,
  onPathClick,
  onApproveApproval,
  onDenyApproval,
  onApproveBatchApproval,
  onDenyBatchApproval,
  onReasoningFoldChange,
}: {
  steps: TimelineStep[];
  isStreaming?: boolean;
  /**
   * Keep the block expanded even when `isStreaming` is false — the
   * agent loop is paused (pending approval / running tool) and has not
   * produced its final output yet. Only the truly idle/final state
   * collapses the thinking block.
   */
  keepOpen?: boolean;
  hasRunningTool: boolean;
  /** Wall-clock time the LLM took for this turn (ms). `undefined` =
   *  no data — reloaded history never has it, so the header just says
   *  "已完成" without a duration. Live streams always have it. */
  durationMs?: number;
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /** Forwarded to inline `kind: "approval_request"` step rendering
   *  (`<ApprovalRequestView>`). Without these the 拒绝 / 允许
   *  buttons are dead. */
  onApproveApproval?: (requestId: string, remember?: boolean) => void;
  onDenyApproval?: (requestId: string) => void;
  /** Forwarded to inline `kind: "approval_request_batch"` step
   *  rendering (`<ApprovalBatchView>`). One batch card resolves
   *  N parallel high-risk tool calls with a single click. */
  onApproveBatchApproval?: (batchId: string, remember?: boolean) => void;
  onDenyBatchApproval?: (batchId: string) => void;
  /** Fires every time `expanded` flips — used by the parent to
   *  decide whether the bottom-of-chat `<ReasoningTicker>` should
   *  show (only when the user has folded the reasoning block). */
  onReasoningFoldChange?: (folded: boolean) => void;
}) {
  // Streaming → always expanded (no fold UI shown at all).
  // Paused (keepOpen) → also expanded: the agent is awaiting approval /
  //   a running tool and has NOT produced its final output yet, so the
  //   thinking block must stay open (collapsing here makes it look like
  //   the turn ended when it has not).
  // Running tool (hasRunningTool) → expanded too: between agent-loop
  //   turns the LLM stream settles (isStreaming=false) while the tool
  //   is still executing — collapsing the thinking block in that window
  //   reads as "agent stopped", which is wrong.
  // Truly settled → default collapsed; user can click to expand.
  const [userOverride, setUserOverride] = useState<boolean | null>(null);
  const expanded = isStreaming || keepOpen || hasRunningTool ? true : (userOverride ?? false);

  // Report fold state up to the parent so the bottom-of-chat
  // `<ReasoningTicker>` can mount when the user has folded this
  // assistant's reasoning block. Default state (before any toggle)
  // counts as folded since that's the visual default on settled
  // turns — the parent only mounts the ticker when there's actual
  // reasoning to display.
  useEffect(() => {
    onReasoningFoldChange?.(!expanded);
  }, [expanded, onReasoningFoldChange]);

  // When a new turn starts (live → settled → live again), drop the
  // user's manual override so the auto-collapse default kicks in.
  useEffect(() => {
    if (!isStreaming) setUserOverride(null);
  }, [isStreaming]);

  // Pre-compute tool groups per step so the "已完成 N 个工具" count
  // reflects actual tool *invocations*, not raw timeline events — a
  // single tool call can emit hundreds of `tool_stdout` chunks that would
  // otherwise inflate the count to 700+ for just a handful of tools.
  // Reusing the same `groupTimeline` call that drives rendering keeps the
  // header count in sync with what the user sees when expanded.
  // During streaming the parent re-renders 30-50×/s (rAF batches), so the
  // memo avoids re-running `groupTimeline` when `steps`/streaming are unchanged.
  // While the live turn is active (streaming text, a tool is still
  // executing, or the UI is paused for approval), do NOT flip stranded
  // groups to `"error"`. A long-running tool whose `tool_complete` has not
  // arrived yet would otherwise flash "执行已中断 / 失败" the moment the
  // LLM text stream settles. Only when the turn is truly idle do we promote
  // genuinely stranded groups to "error" so reloads still surface real
  // interruptions.
  const streamComplete = !(isStreaming || hasRunningTool || keepOpen);
  // Any pending inline approval card? Surface this so `ToolGroupCard`
  // can switch its badge from "运行中" to "等待审批" — the agent is
  // blocked on the user's decision, not actively executing. We don't
  // have a stable tool_call_id → group.key mapping today (server's
  // `RequestApproval` event carries `request_id` but no tool_call_id),
  // so we broadcast the flag to every running group on this turn.
  // That's slightly over-eager when several tools are running and
  // only one is awaiting approval, but the user-perceived fix is
  // identical: the card no longer lies about being mid-execution.
  const awaitingApproval = steps.some(
    (s) => s.kind === "approval_request" && s.meta.decision === null,
  );
  // 注：旧版“自动滚动到待审批卡片”的逻辑已随内联审批卡一起移除 ——
  // 待决定审批改由 ApprovalDrawer 承载，它固定在输入框上方（视线落点），
  // 不存在“渲染在视野外”的问题。
  const stepGroups = useMemo(
    () =>
      steps.map((s) =>
        s.kind === "tools"
          ? groupTimeline(s.events, { streamComplete })
          : null,
      ),
    // `steps` is a new array on every render during streaming
    // (the parent rebuilds it from the live message's unified_steps),
    // so the memo will recompute when steps actually change. When the
    // stream settles (`isStreaming` flips false), the groups must also
    // recompute because `streamComplete` changes stranded→error.
    [steps, isStreaming],
  );

  if (steps.length === 0) return null;

  // Whether this turn has any reasoning/tool/approval-process scaffolding.
  // Pure-text turns render with no fold header (text flows as plain
  // prose). The fold header now wraps everything non-text — reasoning,
  // tool cards, AND resolved approvals — so the final reply text stays
  // scannable while the agent's internal process collapses behind one
  // tap (Claude Code style).
  const hasProcessContent = steps.some(
    (s) =>
      s.kind === "reasoning" ||
      s.kind === "tools" ||
      (s.kind === "approval_request" && s.meta.decision !== null),
  );

  const totalTools = stepGroups.reduce(
    (acc, groups) => acc + (groups ? groups.length : 0),
    0,
  );
  const reasoningSteps = steps.filter((s) => s.kind === "reasoning");
  const resolvedApprovals = steps.filter(
    (s) => s.kind === "approval_request" && s.meta.decision !== null,
  ).length;

  // 工具输出的总行数。折叠态下用户看不到内容，"这一轮翻了多少东西"
  // 是判断要不要展开的唯一线索，所以和工具数一起放在摘要行。
  // 只数 tool_complete 的最终结果（tool_stdout 是分片，逐片数会把
  // 行数放大数倍）；历史消息无结果体时自然为 0，不显示。
  const totalOutputLines = steps.reduce((acc, s) => {
    if (s.kind !== "tools") return acc;
    for (const e of s.events) {
      if (e.type !== "tool_complete") continue;
      if (typeof e.content !== "string" || !e.content) continue;
      acc += e.content.split("\n").length;
    }
    return acc;
  }, 0);

  // Header label: only used when settled (isStreaming=false). Live
  // renders no header — the content flows inline like prose.
  // Aggregate "思考 + 工具 + 审批" into one tight chip:
  // "已完成 5 个工具 7s"     ← tools only
  // "已完成 7s"               ← pure reasoning/text
  // "思考 3s · 5 个工具"      ← both (compact form)
  const durationPart = formatDuration(durationMs);
  const thinkSec = reasoningSteps.length > 0
    ? formatDuration(durationMs)
    : null;
  // 失败信息不再拼进顶头文案（避免刺眼的"N 失败"banner），
  // 由折叠头的 AlertTriangle 图标低调表示即可。
  const settledLabel = keepOpen
    ? (totalTools > 0 ? `处理中 ${totalTools} 个工具` : "处理中")
    : (() => {
      // Multi-source aggregate: pick the most informative combo.
      const parts: string[] = [];
      if (totalTools > 0) {
        parts.push(
          durationPart
            ? `已完成 ${totalTools} 个工具 ${durationPart}`
            : `已完成 ${totalTools} 个工具`,
        );
      } else if (durationPart) {
        parts.push(`已完成 ${durationPart}`);
      } else if (resolvedApprovals > 0) {
        parts.push("已完成");
      } else {
        parts.push("已完成");
      }
      if (reasoningSteps.length > 0 && thinkSec) {
        parts.push(`思考 ${thinkSec}`);
      }
      if (resolvedApprovals > 0 && totalTools === 0 && !durationPart) {
        parts.push(`${resolvedApprovals} 个审批`);
      }
      return parts.join(" · ");
    })();

  // Reasoning and tool steps are kept as siblings, never nested. They
  // share the same fold header (2026-08-18 refactor) — both collapse
  // behind one tap when the turn settles. Text steps stay outside
  // the fold so the final reply is always scannable.
  // (No extra `reasoningSteps` derivation here — already computed
  // above for `settledLabel`.)

  return (
    <div className="tl-process-timeline">
      {/* Reasoning fold header — only visible once the turn has settled.
          During streaming the content flows inline so the user
          perceives a single continuous narrative, not a stack
          of chips. The header toggles *reasoning* visibility only;
          tool cards below are always shown. */}
      {!isStreaming && hasProcessContent && (
        <button
          onClick={() => setUserOverride((v) => !v)}
          className="reasoning-header tl-process-header"
          aria-expanded={expanded}
          title={reasoningSteps.length > 0 ? "思考过程（点击展开/收起）" : settledLabel}
        >
          {expanded ? (
            <ChevronDown className="icon-sm reasoning-chevron reasoning-chevron-open" />
          ) : (
            <ChevronRight className="icon-sm reasoning-chevron" />
          )}
          <CheckCircle2 className="icon-sm tl-process-check" />
          <span className="reasoning-label">
            {reasoningSteps.length > 0 ? "思考" : settledLabel}
          </span>
          {/* 折叠态的"分量提示"：有思考时主标签是"思考"，工具数/行数/耗时
              作为次级统计挂在右侧，避免和思考文案挤成一句话。 */}
          {reasoningSteps.length > 0 && !isStreaming && totalTools > 0 && (
            <span className="reasoning-stat">
              {totalTools} 个工具
              {totalOutputLines > 0 ? ` · ${totalOutputLines} 行` : ""}
              {durationPart ? ` · ${durationPart}` : ""}
            </span>
          )}
        </button>
      )}
      {/* Chronological render:
          - text steps always stay inline at their true position
            (the LLM's narrative, with surrounding markdown etc.)
          - tool steps always render inline too — one
            ToolGroupCard per call — so the user can follow the
            agent's actions in chronological order, interleaved
            with the text it emitted before/after each tool call.
            They are NOT part of the reasoning fold.
          - reasoning stays collapsed by default (settled+closed)
            and only expands live / on user toggle, because LLM
            thinking can be long. */}
      <div className="tl-process-body">
        {steps.map((step, i) => {
          if (step.kind === "text") {
            const parsed = parseContent(step.content);
            return (
              <div key={`text-${i}`} className="tl-iteration tl-text-step">
                {parsed.body && (
                  <div className="tl-text-inner">
                    <MarkdownBody
                      content={parsed.body}
                      onPathClick={onPathClick ?? (() => {})}
                    />
                  </div>
                )}
              </div>
            );
          }
          // Reasoning respects the `expanded` fold — long thinking
          // blocks would otherwise dominate the message body. Tool
          // calls render regardless of fold state (see below).
          if (step.kind === "reasoning") {
            if (!expanded && !isStreaming) return null;
            return <ReasoningBlock key={`ps-${i}`} content={step.content} />;
          }
          if (step.kind === "tools" && stepGroups[i]) {
            const groups = stepGroups[i]!;
            if (groups.length === 0) return null;
            // Process timeline (2026-08-18 refactor): tools are
            // siblings of reasoning under the same fold header —
            // matches Claude Code's single fold for "thinking +
            // acting", so the final reply stays scannable while the
            // internal process (every `ToolGroupCard` + every
            // `ReasoningBlock`) collapses behind one tap.
            if (!expanded && !isStreaming) return null;
            return (
              <div key={`ps-${i}`} className="tl-tool-step">
                <ToolTimelineSection
                  groups={groups}
                  onPathClick={onPathClick}
                  awaitingApproval={awaitingApproval}
                />
              </div>
            );
          }
          // Inline permission card — pushed into the assistant's
          // `unified_steps` by `App.tsx::onApprovalRequest` so the
          // card sits at the chronological position of its tool
          // call (right after `tool_start`, before `tool_complete`).
          // Rendered with the same `ApprovalRequestView` as the
          // legacy `role: "approval"` path so the visual contract
          // is identical.
          //
          // Pending approvals (`meta.decision === null`) are
          // *outside* the fold: hiding them behind a chevron
          // makes the user miss the click — they're the active
          // "waiting" surface. Resolved approvals collapse with
          // everything else.
          if (step.kind === "approval_request") {
            if (step.meta.decision === null) {
              // PENDING — 不再内联渲染。待决定审批改由 ChatView 底部的
              // ApprovalDrawer（输入框上方抽屉）承载：它是"现在需要你
              // 做的决定"，不是流的一部分；埋在流中间（尤其折叠时）会
              // 被用户错过。这里返回 null，抽屉负责让用户看到它。
              return null;
            }
            // RESOLVED — collapses after a brief summary beat during
            // the live turn (avoids the ~1s blank an instant unmount
            // would leave until the agent loop's next WS event arrives);
            // hidden behind the fold only when the turn has settled.
            // Wrapper gets `.tl-approval--leaving` while still live so
            // the CSS keyframe (600ms delay → 280ms fold) drives the
            // collapse. `forwards` keeps the card at 0 height for the
            // rest of the turn — no flicker on the next streamed chunk.
            const turnActive = isStreaming || expanded;
            if (!turnActive) return null;
            const showLeaving = isStreaming && !expanded;
            return (
              <div
                key={`ps-${i}`}
                className={`tl-approval-step${showLeaving ? " tl-approval--leaving" : ""}`}
              >
                <ApprovalRequestView
                  meta={step.meta}
                  onApprove={onApproveApproval}
                  onDeny={onDenyApproval}
                />
              </div>
            );
          }
          if (step.kind === "approval_request_batch") {
            // Batch — 与单个审批同策略：待决定的整批搬到 ApprovalDrawer
            // （输入框上方抽屉），这里只在已决定后渲染历史记录。
            if (step.meta.decision === null) {
              return null;
            }
            const turnActive = isStreaming || expanded;
            if (!turnActive) return null;
            const showLeaving = isStreaming && !expanded;
            return (
              <div
                key={`ps-${i}`}
                className={`tl-approval-step tl-approval-step--batch${showLeaving ? " tl-approval--leaving" : ""}`}
              >
                <ApprovalBatchView
                  meta={step.meta}
                  onApprove={onApproveBatchApproval}
                  onDeny={onDenyBatchApproval}
                />
              </div>
            );
          }
          return null;
        })}
      </div>
    </div>
  );
}
export function ToolTimelineSection({
  groups,
  isStreaming: _isStreaming,
  onPathClick,
  awaitingApproval = false,
}: {
  groups: import("./messageParser").ToolGroup[];
  isStreaming?: boolean;
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  /** Forwarded to each `ToolGroupCard`. See ToolGroupCard for the
   *  full rationale (replace "运行中" with "等待审批" so the user
   *  knows the agent is blocked on their decision, not stuck). */
  awaitingApproval?: boolean;
}) {
  if (groups.length === 0) return null;
  return (
    <div className="tl-tools tl-tools--flat">
      <div className="tl-tools-list">
        {groups.map((g) => (
          <ToolGroupCard key={g.key} group={g} onPathClick={onPathClick} awaitingApproval={awaitingApproval} />
        ))}
      </div>
    </div>
  );
}
export function UnifiedTimelineRenderer({
  steps,
  isStreaming,
  keepOpen = false,
  onPathClick,
  fileChanges,
  memoryUpdates,
  memoryRecalls,
  toolCalls,
  hasRunningTool,
  durationMs,
  onApproveApproval,
  onDenyApproval,
  onApproveBatchApproval,
  onDenyBatchApproval,
  onReasoningFoldChange,
}: {
  steps: TimelineStep[];
  isStreaming?: boolean;
  /** Forwarded to ProcessTimeline: keep the reasoning/tool block
   *  expanded while the agent is paused (pending approval / running
   *  tool) but the text stream has settled. */
  keepOpen?: boolean;
  onPathClick?: (path: string, e: React.MouseEvent) => void;
  fileChanges: FileChangeEntry[];
  memoryUpdates: MemoryUpdateEntry[];
  memoryRecalls: MemoryRecallEntry[];
  toolCalls: NonNullable<ChatMessage["tool_calls"]>;
  hasRunningTool: boolean;
  /** Forwarded to ProcessTimeline for the "已完成 10s" header chip. */
  durationMs?: number;
  /** Forwarded to ProcessTimeline for the inline
   *  `kind: "approval_request"` step's 拒绝 / 允许 buttons. */
  onApproveApproval?: (requestId: string) => void;
  onDenyApproval?: (requestId: string) => void;
  /** Forwarded to ProcessTimeline for the batch approval step
   *  (`<ApprovalBatchView>`). */
  onApproveBatchApproval?: (batchId: string, remember?: boolean) => void;
  onDenyBatchApproval?: (batchId: string) => void;
  /** Fires every time `expanded` flips — used by the parent to
   *  decide whether the bottom-of-chat `<ReasoningTicker>` should
   *  show (only when the user has folded the reasoning block). */
  onReasoningFoldChange?: (folded: boolean) => void;
}) {
  const hasTools = toolCalls.length > 0;
  const hasAnyToolStep = steps.some((s) => s.kind === "tools");
  const hasAnyTextStep = steps.some((s) => s.kind === "text");
  const hasAnyReasoningStep = steps.some((s) => s.kind === "reasoning");

  // Steps render chronologically inside ProcessTimeline — text stays
  // at its true position, reasoning + tools collapse by default. No
  // separate "process" vs "text" split (that detached tool cards from
  // the sentences that preceded/followed them).

  // If all steps are empty (no reasoning, no tools, no text), render
  // the streaming placeholder (ThinkingDots) or nothing.
  if (steps.length === 0) {
    if (isStreaming && !hasTools) {
      return (
        <div className="tl-iterations">
          <div className="tl-iteration">
            <ThinkingDots label="正在处理" />
          </div>
        </div>
      );
    }
    return null;
  }

  return (
    <div className="tl-iterations">
      {/* One chronological stream: reasoning + tools collapse behind a
          single header; text stays inline at its true position. */}
      <ProcessTimeline
        steps={steps}
        isStreaming={isStreaming}
        keepOpen={keepOpen}
        hasRunningTool={hasRunningTool}
        durationMs={durationMs}
        onPathClick={onPathClick}
        onApproveApproval={onApproveApproval}
        onDenyApproval={onDenyApproval}
        onApproveBatchApproval={onApproveBatchApproval}
        onDenyBatchApproval={onDenyBatchApproval}
        onReasoningFoldChange={onReasoningFoldChange}
      />

      {/* Trailing meta blocks — file changes / memory updates /
          recalls. Rendered once after the stream, attached to the
          final reply (the last text step is the terminal node in the
          common flow). Only when the turn produced text. */}
      {hasAnyTextStep && fileChanges.length > 0 && (
        <div className="tl-text-inner tl-text-inner--meta">
          <FileChangeBlock changes={fileChanges} onPathClick={onPathClick} />
        </div>
      )}
      {hasAnyTextStep && memoryUpdates.length > 0 && (
        <div className="tl-text-inner tl-text-inner--meta">
          <MemoryUpdateBlock updates={memoryUpdates} />
        </div>
      )}
      {hasAnyTextStep && memoryRecalls.length > 0 && (
        <div className="tl-text-inner tl-text-inner--meta">
          <MemoryRecallBlock recalls={memoryRecalls} />
        </div>
      )}

      {/* Legacy fallback: running tool calls without timeline events.
          Only shown when there are tool_calls but no tool steps
          (pre-migration data) and at least one is still running. */}
      {!hasAnyToolStep && toolCalls.length > 0 && hasRunningTool && (
        <div className="tl-iteration">
          <ToolCallsFallback calls={toolCalls} />
        </div>
      )}

      {/* Gap thinking animation: only shown when tools are running
          but we have NOT yet rendered any tool cards.
          NOTE: must also require isStreaming — once the stream settles,
          hasRunningTool becomes stale (activeTurnMsgIds clears on next
          react cycle) and showing "正在处理…" on a settled message
          contradicts the idle WaitForInputIndicator below it. */}
      {isStreaming && hasRunningTool && !hasTools && !hasAnyReasoningStep && (
        <div className="tl-iteration">
          <ThinkingDots label="正在处理" />
        </div>
      )}

      {/* All tools done, body still empty → waiting for final text.
          Only fire for the LIVE streaming message. */}
      {hasAnyToolStep && !hasRunningTool && !hasAnyTextStep && isStreaming && (
        <div className="tl-iteration">
          <ThinkingDots label="等待回复" />
        </div>
      )}
    </div>
  );
}
export function FileChangeBlock({
  changes,
  onPathClick,
}: {
  changes: FileChangeEntry[];
  onPathClick?: (path: string, e: React.MouseEvent) => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const VISIBLE = 4;
  const visible = expanded ? changes : changes.slice(0, VISIBLE);
  const hidden = Math.max(0, changes.length - VISIBLE);
  return (
    <div className="file-change-block footer-block">
      <div className="file-change-header">
        <FileText className="icon-sm shrink-0" />
        <span>文件变更</span>
        <span className="file-change-count">（{changes.length}个）</span>
      </div>
      <ul className="file-change-list">
        {visible.map((c, i) => {
          const filename = c.path.split(/[\\/]/).pop() ?? c.path;
          return (
            <li key={`${c.path}-${i}`}>
              <button
                type="button"
                className="file-change-link"
                title={c.path}
                onClick={(e) => onPathClick?.(c.path, e)}
              >
                {filename}
              </button>
            </li>
          );
        })}
      </ul>
      {!expanded && hidden > 0 && (
        <button
          type="button"
          className="file-pill-toggle"
          onClick={() => setExpanded(true)}
>
          查看全部变更
          <ArrowRight className="icon-sm" />
        </button>
      )}
      {expanded && hidden > 0 && (
        <button
          type="button"
          className="file-pill-toggle"
          onClick={() => setExpanded(false)}
        >
          收起
          <ChevronDown className="icon-sm rotate-180" />
        </button>
      )}
    </div>
  );
}
export function MemoryUpdateBlock({ updates }: { updates: MemoryUpdateEntry[] }) {
  const iconFor = (action: MemoryUpdateEntry["action"]) => {
    switch (action) {
      case "write": return <FilePlus className="icon-sm shrink-0" />;
      case "update": return <Edit3 className="icon-sm shrink-0" />;
      case "delete": return <Trash2 className="icon-sm shrink-0" />;
      case "summarize": return <Brain className="icon-sm shrink-0" />;
      case "cleanup": return <Trash2 className="icon-sm shrink-0" />;
      case "snapshot": return <Database className="icon-sm shrink-0" />;
    }
  };
  const labelFor = (action: MemoryUpdateEntry["action"]) => {
    switch (action) {
      case "write": return "记忆写入";
      case "update": return "记忆更新";
      case "delete": return "记忆删除";
      case "summarize": return "记忆总结";
      case "cleanup": return "记忆清理";
      case "snapshot": return "记忆快照";
    }
  };
  // Action tint (text color). Kept within the gray/zinc/amber/green/red
  // palette — never blue/purple.
  const tintFor = (action: MemoryUpdateEntry["action"]) => {
    switch (action) {
      case "write": return "text-emerald-400";
      case "update": return "text-amber-400";
      case "delete": return "text-red-400";
      case "summarize": return "text-zinc-300";
      case "cleanup": return "text-red-400";
      case "snapshot": return "text-zinc-400";
    }
  };
  return (
    <div className="memory-update-block">
      <div className="memory-update-header">
        <Brain className="icon-sm shrink-0 text-zinc-400" />
        <span>记忆</span>
        <span className="memory-update-count">（{updates.length}条）</span>
      </div>
      <ul className="memory-update-list">
        {updates.map((u, i) => (
          <li key={`${u.action}-${u.key}-${i}`} className="memory-update-item">
            <span className={`memory-update-badge ${tintFor(u.action)}`}>
              {iconFor(u.action)}
              {labelFor(u.action)}
            </span>
            <span className="memory-update-text" title={u.summary}>
              {u.summary}
              {typeof u.count === "number" && u.count > 1 ? (
                <span className="memory-update-num"> · {u.count}</span>
              ) : null}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
export function MemoryRecallBlock({ recalls }: { recalls: MemoryRecallEntry[] }) {
  const labelFor = (t: MemoryRecallEntry["trigger"]) => {
    switch (t) {
      case "first_turn": return "首轮热身";
      case "interval": return "周期召回";
      case "drift": return "主题漂移";
    }
  };
  return (
    <div className="memory-recall-block">
      {recalls.map((r, i) => {
        const keys = r.previewKeys.slice(0, 3).join(", ");
        const more = r.previewKeys.length > 3 ? ` …+${r.previewKeys.length - 3}` : "";
        const chars = typeof r.totalChars === "number" && r.totalChars > 0
          ? ` · ${r.totalChars} 字`
          : "";
        return (
          <div
            key={`${r.trigger}-${r.turnNumber}-${i}`}
            className="memory-recall-row"
            title={`第 ${r.turnNumber} 轮 ${labelFor(r.trigger)}`}
          >
            <Brain className="icon-sm shrink-0 text-zinc-500" />
            <span className="memory-recall-label">
              第 {r.turnNumber} 轮 想到 {r.count} 条
            </span>
            <span className="memory-recall-trigger">
              {labelFor(r.trigger)}
            </span>
            {keys ? (
              <span className="memory-recall-keys">
                {keys}{more}
              </span>
            ) : null}
            {chars ? (
              <span className="memory-recall-chars">{chars}</span>
            ) : null}
          </div>
        );
      })}
    </div>
  );
}
export function ToolMessageCard({
  message,
  workspaceRoot: _workspaceRoot,
  onPathClick,
}: {
  message: ChatMessage;
  workspaceRoot?: string | null;
  onPathClick?: (path: string, e: React.MouseEvent) => void;
}) {
  const meta = message.tool;
  if (!meta) return null;
  const Icon = toolIcon(meta.tool);
  const isRunning = meta.status === "running";
  const isError = meta.status === "error";
  const [open, setOpen] = useState(false);
  const hasOutput = meta.output != null && meta.output.length > 0;
  // Memory tool: lift the structured JSON result so the row reads
  // `<动作> <记忆文件路径> <状态>` and the path is clickable to a
  // preview. no-match / ambiguous are soft outcomes ("未查到对应内容"
  // / "匹配到多条"), never a red "失败" badge.
  const memResult =
    meta.tool === "memory" ? parseMemoryResult(meta.output) : null;
  // Two-piece label: `<verb> <object>` (e.g. "读取 PDF D:\data\发票汇总.xls").
  // - `verb` comes from `toolVerb` (registered Chinese verb, or the
  // literal English name for unknown tools).
  // - `object` is `argsHint` — the most meaningful string we
  // can lift from the tool's args (path / url / command / query).
  // If `argsHint` is empty (e.g. a no-arg tool), we just show
  // the verb alone so the row doesn't read "读取文件 (空)".
  const verb = memResult
    ? memoryActionLabel(memResult.action)
    : toolVerb(meta.tool, meta.args);
  // The row keeps two distinct slots — verb (left) and primary (middle,
  // optional) — so they never merge into a duplicated string like
  // "调用工具 D:\path D:\path". The primary slot shows the most meaningful
  // arg / stdout (path, URL, command, query) or, on error, a one-line
  // summary instead of raw JSON.
  const primary = memResult
    ? memResult.path
    : isError
      ? (summarizeError(meta.output) ?? toolPrimaryArg(meta.tool, meta.args, meta.output))
      : toolPrimaryArg(meta.tool, meta.args, meta.output);
  const stats = extractLineStats(meta.tool, meta.args);
  // Path-shaped primary arg is clickable.  We only auto-recognise
  // the standard file ops; everything else (e.g. python scripts,
  // arbitrary text) is left as plain text to avoid spurious "open
  // file" affordances.
  const isPathLike = memResult ? true : isPathArg(meta.tool, meta.args, meta.output);
  const primaryPath = memResult
    ? memResult.path
    : isPathLike ? (meta.output || meta.args) : null;
  const handlePrimaryClick = (e: React.MouseEvent) => {
    // Memory tool: the path is always a real file — open it directly.
    if (memResult?.path && onPathClick) {
      e.stopPropagation();
      onPathClick(memResult.path, e);
      return;
    }
    if (isPathLike && primaryPath && onPathClick) {
      const path = extractFirstPath(primaryPath);
      if (path) {
        e.stopPropagation();
        onPathClick(path, e);
        return;
      }
    }
    if (hasOutput) setOpen((v) => !v);
  };

  // Result peek — a one-line human-readable summary of what the
  // tool produced (or why it failed). Extracted from the output
  // in priority order: error message → success summary → first line.
  const resultPeek = useMemo(() => {
    // Memory tool: the status badge already states the outcome
    // ("未查到对应内容" / "成功" / …); keep the row to a single line
    // and let the expandable body carry the full message.
    if (memResult) return null;
    if (isError) {
      return meta.errorReason
        ?? summarizeError(meta.output)
        ?? (meta.output ? toolResultPeek(meta.output, meta.args) : null)
        ?? "执行出错";
    }
    if (!meta.output || meta.output.trim().length === 0) return null;
    // For running tools, don't show a result peek (it's not done yet)
    if (isRunning && !meta.completedAt) return null;
    return toolResultPeek(meta.output, meta.args);
  }, [meta.output, meta.args, meta.status, meta.completedAt, meta.errorReason, isError, isRunning]);

  // Archive chip for memory operations that triggered auto-archive.
  const archiveChip = useMemo(
    () => memoryArchiveChip(meta.tool, meta.output),
    [meta.tool, meta.output],
  );

  return (
    <div className="tw-msg h-enter chat-tool-call chat-tool-call--tool">
      <button
        type="button"
        onClick={hasOutput ? () => setOpen((v) => !v) : undefined}
        className="tw-msg-row"
        data-tool-kind={toolKind(meta.tool)}
        data-state={isError ? "err" : isRunning ? "run" : "ok"}
      >
        <span className="tw-msg-icon-tile">
          <Icon className="icon-sm tw-msg-icon-svg" />
        </span>

        {/* Verb (e.g. "执行命令") — right next to the icon tile */}
        <span className="tw-msg-verb">{verb}</span>
        {primary && (
          <span
            className={isPathLike ? "tw-msg-primary tw-msg-primary--link" : "tw-msg-primary"}
            title={isPathLike ? `点击打开 ${primary}` : primary}
            onClick={isPathLike ? handlePrimaryClick : undefined}
          >
            {primary}
          </span>
        )}

        {/* Result peek — compact one-line summary between primary
            arg and the right-side status group. Only shown for
            completed tools with a meaningful result.
            Format:   | tail 不是内部或外部命令 (error - red)
                      | 已写入 823 bytes (success - muted)
                      | 找到 12 个结果 (search - muted) */}
        {resultPeek && !isError && (
          <span className="tw-msg-result-peek">{resultPeek}</span>
        )}
        {archiveChip && (
          <span className="tw-msg-chip tw-msg-archive-chip" title="记忆已自动归档旧条目">
            {archiveChip}
          </span>
        )}

        {/* Right side: line stats + status indicator, kept inline at the
            message tail (not space-between) so it isn't pulled apart. */}
        <span className="tw-msg-right chat-status-inline">
          {(stats.added != null || stats.removed != null) && (
            <span className="tw-msg-stats">
              {stats.added != null && stats.added > 0 && (
                <span className="tw-msg-stat tw-msg-stat--add">+{stats.added}</span>
              )}
              {stats.removed != null && stats.removed > 0 && (
                <span className="tw-msg-stat tw-msg-stat--del">−{stats.removed}</span>
              )}
            </span>
          )}
          {/* Status uses small icons + text in muted / success / warning
              tones — never raw blue/red characters: 已完成 → Check;
              失败 / 已中断 → AlertTriangle; 运行中 → Loader. */}
          {(() => {
            // Memory tool: render the structured status as the badge
            // — "未查到对应内容" / "匹配到多条" / "成功" / "失败".
            // no-match / ambiguous are soft (muted / amber), never red.
            const memBadge = (st: string) => {
              switch (st) {
                case "success":
                  return <><Check className="icon-sm shrink-0" /> 成功</>;
                case "not_found":
                  return <><SearchX className="icon-sm shrink-0" /> 未查到对应内容</>;
                case "ambiguous":
                  return <><AlertTriangle className="icon-sm shrink-0" /> 匹配到多条</>;
                case "error":
                  return <><AlertCircle className="icon-sm shrink-0" /> 失败</>;
                default:
                  return <><Check className="icon-sm shrink-0" /> 成功</>;
              }
            };
            if (memResult && !isRunning) {
              return (
                <span className={`tw-msg-state ${memoryStatusColor(memResult.status)}`}>
                  {memBadge(memResult.status)}
                </span>
              );
            }
            return (
              <span className={`tw-msg-state tw-msg-state--${isError ? "err" : isRunning ? "run" : "done"}`}>
                {isRunning ? (
                  <>
                    <Loader2 className="icon-sm animate-spin shrink-0" />
                    运行中
                  </>
                ) : isError ? (
                  <>
                    <AlertCircle className="icon-sm shrink-0" />
                    失败
                  </>
                ) : (
                  <>
                    <Check className="icon-sm shrink-0" />
                    已完成
                  </>
                )}
              </span>
            );
          })()}
          {hasOutput && (
            <span className="tw-msg-chev">{open ? <ChevronDown className="icon-sm" /> : <ChevronRight className="icon-sm" />}</span>
          )}
        </span>
      </button>

      {/* Error snippet — shown only when the primary text doesn't
          already contain the error summary. This prevents duplicate
          red text (primary shows "Binary file detected..." and the
          err-hint below shows the same string). */}
      {isError && resultPeek && primary !== resultPeek && (
        <div
          className="tw-msg-err-hint"
          onClick={() => setOpen(true)}
          title="点击展开完整输出"
        >
          <AlertCircle size={10} className="shrink-0" />
          <span className="truncate">{resultPeek}</span>
          <ChevronRight className="icon-sm shrink-0 tw-msg-chev-err" />
        </div>
      )}

      {/* Expandable output body — suppressed for error tools.
          The error snippet above (tw-msg-err-hint) already shows
          what went wrong; expanding to the full red body just
          repeats the same content.

          For `cua_screenshot`, the image preview is rendered
          INLINE (always visible, no expand needed) — the user
          almost always wants to *see* the screenshot, not read
          the raw base64 payload. A second collapsible block
          under the image lets them peek at the JSON metadata if
          they care about coordinates / file size. */}
      {!isError && meta.tool === "cua_screenshot" && (
        <ScreenshotPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_list_windows" && (
        <WindowListPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_window_at_point" && (
        <WindowAtPointPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_ui_tree" && (
        <UiTreePreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_ui_find" && (
        <UiFindPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_ui_click" && (
        <UiClickPreview output={meta.output} />
      )}
      {!isError && meta.tool === "cua_ui_focus" && (
        <UiSingleNamePreview output={meta.output} verb="聚焦" />
      )}
      {!isError && meta.tool === "cua_ui_invoke" && (
        <UiSingleNamePreview output={meta.output} verb="触发" />
      )}
      {hasOutput && open && !isError && meta.tool !== "cua_screenshot" && meta.tool !== "cua_list_windows" && meta.tool !== "cua_window_at_point" && meta.tool !== "cua_ui_tree" && meta.tool !== "cua_ui_find" && meta.tool !== "cua_ui_click" && meta.tool !== "cua_ui_focus" && meta.tool !== "cua_ui_invoke" && (
        <div className="tw-msg-body">
          <LiveTerminal
            content={meta.output!}
            running={isRunning && !meta.completedAt}
          />
        </div>
      )}
    </div>
  );
}
export function LiveTerminal({ content, running }: { content: string; running: boolean }) {
  // Unwrap JSON result wrappers so the user sees clean terminal output.
  const displayContent = useMemo(() => unwrapToolOutput(content), [content]);
  // Copy-to-clipboard state. ~1.4s "已复制" flash so the affordance
  // gives clear feedback without nagging the user with a toast.
  const [copied, setCopied] = useState(false);
  const copyTimerRef = useRef<number | null>(null);

  // Cancel any pending "copied" reset on unmount so we don't fire
  // setState on a dead component.
  useEffect(() => () => {
    if (copyTimerRef.current) window.clearTimeout(copyTimerRef.current);
  }, []);

  const handleCopy = async () => {
    const text = displayContent || (running ? "" : "（无输出）");
    if (!text) return;
    try {
      if (navigator.clipboard?.writeText) {
        await navigator.clipboard.writeText(text);
      } else {
        // Tauri webview fallback (e.g. http context without the
        // modern clipboard API): hidden textarea + execCommand.
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.position = "fixed";
        ta.style.left = "-9999px";
        document.body.appendChild(ta);
        ta.select();
        document.execCommand("copy");
        document.body.removeChild(ta);
      }
      setCopied(true);
      if (copyTimerRef.current) window.clearTimeout(copyTimerRef.current);
      copyTimerRef.current = window.setTimeout(() => setCopied(false), 1400);
    } catch {
      // Silent failure — the user can still select the text manually.
    }
  };

  const hasContent = displayContent.length > 0;

  return (
    <div
      className="tw-msg-output"
      data-running={running ? "true" : "false"}
      data-empty={hasContent ? "false" : "true"}
    >
      <button
        type="button"
        className="tw-msg-output-copy"
        onClick={(e) => { e.stopPropagation(); handleCopy(); }}
        data-copied={copied ? "true" : "false"}
        title={copied ? "已复制" : "复制输出"}
        aria-label={copied ? "已复制" : "复制输出"}
      >
        {copied ? <CheckCheck size={10} /> : <Copy size={10} />}
      </button>
      {hasContent ? (
        <pre className="tw-msg-output-text">{displayContent}</pre>
      ) : (
        <span className="tw-msg-output-empty">
          {running ? "等待输出…" : "（无输出）"}
        </span>
      )}
    </div>
  );
}
export function StatusPill({ status }: { status: "running" | "done" | "error" | undefined }) {
  // Defense-in-depth: any undefined/garbage status is treated as a
  // completed tool call. The historical bug was that wire-loaded
  // `tool_calls` arrived without a `status` field (Rust's `ToolCall`
  // struct doesn't carry one) and StatusPill's ternary fell through
  // to "运行中". Even with the projection in `api.ts#getSessionMessages`,
  // some legacy code paths still pass `tc.status` straight through —
  // this guard makes the whole component fail-safe.
  const safe: "running" | "done" | "error" =
    status === "running" || status === "done" || status === "error"
      ? status
      : "done";
  const cls =
    safe === "done" ? "pill pill-success" :
    safe === "error" ? "pill pill-error" :
    "pill pill-warning";
  const label = safe === "done" ? "完成" : safe === "error" ? "失败" : "运行中";
  return (
    <span className={cls}>
      {safe === "running" && <Loader2 size={9} className="animate-spin" />}
      {label}
    </span>
  );
}
export function ToolCallsFallback({ calls }: { calls: NonNullable<ChatMessage["tool_calls"]> }) {
  return (
    <div className="tool-rail">
      {calls.map((tc, i) => (
        <div key={`tc-${tc.name || i}`} className="tool-row tool-done">
          <span className="rail-dot" />
          <div className="tool-head cursor-default">
            <Wrench className="icon-sm text-text-muted" />
            <span className="tool-name">{tc.name}</span>
            <StatusPill status={tc.status} />
          </div>
        </div>
      ))}
    </div>
  );
}

