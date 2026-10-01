// Tool-call related bubbles: tool-row cards, the unified/process timeline
// renderer, the live terminal, and the file-change / memory meta blocks.
// Extracted from MessageBubble.tsx.
import { buildActivitySummary } from "./chat/activitySummary";
import React, { useState, useEffect, useMemo } from "react";
import {
  ChevronDown, ChevronRight, Loader2,
  FileText, FilePlus, ArrowRight, Edit3, Trash2, Brain,
  Database, Wrench,
} from "lucide-react";
import { MarkdownBody } from "./MarkdownBody";
import type { ChatMessage, TimelineStep } from "../types";
import { ApprovalRequestView } from "./ApprovalBubble";
import { ApprovalBatchView } from "./ApprovalBatchBubble";
import {
  parseContent, groupTimeline,
  type FileChangeEntry, type MemoryUpdateEntry, type MemoryRecallEntry,
} from "./messageParser";
import { parseMemoryResult } from "../utils/memoryToolResult";
import { formatDuration } from "./bubbleUtils";
import { ThinkingDots, ReasoningBlock } from "./ThinkingBubble";
import { ProcessLine, type ProcessLineVM, type ProcessStatus } from "./ProcessLine";

/**
 * 运行态的一行过程。只负责把 `ToolGroup` 映射成 `ProcessLineVM`，
 * 渲染委托给 `<ProcessLine>` —— 与恢复态的 `ToolMessageCard` 共用
 * 同一份 JSX。两条路径的差异到此为止。
 */
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
  const vm = useMemo(() => {
    // Memoize so large stdout outputs aren't re-joined every render —
    // a tool with 500 chunks would otherwise concatenate 500 strings on
    // each parent re-render for an identical result.
    const startEv = group.events.find((e) => e.type === "tool_start");
    const completeEv = group.events.find((e) => e.type === "tool_complete");
    const stdout = group.events
      .filter((e) => e.type === "tool_stdout")
      .map((e) => e.content ?? "")
      .join("");
    const output = completeEv?.content ?? stdout ?? "";
    return {
      tool: group.tool,
      args: startEv?.arguments,
      output,
      status: group.status as ProcessStatus,
      statusNote: group.statusNote,
      awaitingApproval,
      memoryResult: parseMemoryResult(output),
    };
  }, [group.events, group.tool, group.status, group.statusNote, awaitingApproval]);

  return <ProcessLine vm={vm} onPathClick={onPathClick} />;
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
  // 2026-09-29 的展开判定：按「这一组是不是活跃轮次的尾巴」判，
  // **不按「有没有工具正在执行」**判。
  //
  // 改之前这里把 `hasRunningTool` 也算进自动展开条件。问题在两个工具之间：
  // 上一个工具刚 complete、下一个还没 start 的那一瞬，`isStreaming` 是 false
  // 且 `hasRunningTool` 也是 false，折叠条会闪一下收起来再弹开。
  //
  // 2026-10-01 重定：折叠的粒度是「整轮」，展开粒度是「每段」。
  //
  //   还在流（isStreaming）→ **不渲染总折叠头**，过程按真实顺序平铺；
  //     每段思考各自是折叠项，正在想的那段展开、刚结束的那段自动收起。
  //   真的结束 → 整轮折成顶部一行（`已工作 N 分 M 秒`），下面只剩正文。
  //
  // 此前这里 `expanded = isStreaming || keepOpen ? true : …`，导致运行中
  // 思考永远展开且不可收 —— 正是用户截图里「多段思考糊成一片」的成因。
  const [userOverride, setUserOverride] = useState<boolean | null>(null);
  const settled = !(isStreaming || keepOpen);
  const expanded = settled ? (userOverride ?? false) : true;

  // Report fold state up to the parent so the bottom-of-chat
  // `<ReasoningTicker>` can mount when the user has folded this
  // assistant's reasoning block. Default state (before any toggle)
  // counts as folded since that's the visual default on settled
  // turns — the parent only mounts the ticker when there's actual
  // reasoning to display.
  useEffect(() => {
    onReasoningFoldChange?.(!expanded);
  }, [expanded, onReasoningFoldChange]);

  // 2026-09-29：不再「新一轮开始就丢掉用户覆盖」。消息列用了
  // `content-visibility: auto`，行滚出视口会被回收再挂载 —— 折叠状态放
  // 在组件里会被无声吞掉。改为按本轮的稳定 key 存进模块级 Map
  // （见 chat/activitySummary.ts 的 useDisclosure）。

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

  // 早退必须放在**所有** hooks 之后（下面还有 segments useMemo）——
  // 夹在两个 useMemo 之间时，steps 从空变非空会让钩子数量从 3 变 4，
  // React 报 "Rendered more hooks than during the previous render"。

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

  // 2026-09-29：抄 cc-haha `activityGroupModel.ts` 的动词表。摘要要读起来
  // 像一句话（「思考、读取了 5 个文件、执行了 2 条命令」），而不是计数器的
  // 输出（「已完成 7 个工具」）。文件类按去重后的路径数算，不是调用次数。
  // 2026-10-01：改用 buildActivitySummary —— 它把失败 / 跳过**单独统计**。
  // 旧版把失败混进「已完成 5 个工具」，读起来就是全部成功。
  const summary = useMemo(() => buildActivitySummary(steps), [steps]);
  // 旧版的 `shouldSummarize`（单次工具调用不生成摘要）随折叠头重写一起
  // 删掉了：折叠头的主标签现在是**耗时**而不是动词聚合，「已工作 3s ·
  // 读取了 1 个文件」比什么都不显示更有用，而且那一行本来就要能点开。
  const activityLabel = summary.segments.length > 0 ? summary.segments.join("、") : settledLabel;
  // 有失败/跳过时折叠头整行染 error 色，且计数单列在末尾 —— 绝不被
  // 「读取了 4 个文件」这类成功动词淹没。
  // 2026-10-01 第二轮撤销：折叠头改回安静（只说「工作完成 · 共耗时」），
  // 失败计数移到 hover title。见 `foldTitle` 与下方 JSX 注释。
  // 折叠掉的明细走 hover title —— 这些是「想知道才看」的信息，
  // 常驻在折叠头上只会把它挤成一行密文。
  //
  // 失败 / 跳过**在这里也要说**。折叠头已经不显示它们了（见下方 JSX
  // 注释），如果连 tooltip 也不说，「执行了 6 条命令」就在替 2 次失败
  // 背书 —— 一句会在用户真正需要时误导他的谎。折中：折叠头安静，
  // tooltip 诚实。
  const foldTitle = [
    reasoningSteps.length > 0 && `思考 ${reasoningSteps.length} 段`,
    activityLabel,
    summary.failed > 0 && `${summary.failed} 个失败`,
    summary.skipped > 0 && `${summary.skipped} 个跳过`,
    totalOutputLines > 0 && `${totalOutputLines} 行输出`,
    "点击展开/收起",
  ]
    .filter(Boolean)
    .join(" · ");

  if (steps.length === 0) return null;


  // Reasoning and tool steps are kept as siblings, never nested. They
  // share the same fold header (2026-08-18 refactor) — both collapse
  // behind one tap when the turn settles. Text steps stay outside
  // the fold so the final reply is always scannable.
  // (No extra `reasoningSteps` derivation here — already computed
  // above for `settledLabel`.)

  return (
    <div className="tl-process-timeline">
      {/* 整轮折叠头 —— **只在轮次结束后出现**。
          运行中不渲染它：那时的界面是「每段思考各自折叠 + 工具行平铺」
          的实时视图，加一个总折叠头会让人误以为整轮已经结束。
          结束后多段思考 + 全部执行过程收成这一行，下面只剩正文。 */}
      {settled && hasProcessContent && (
        <button
          onClick={() => setUserOverride((v) => !v)}
          className="reasoning-header tl-process-header"
          data-has-failure={summary.failed > 0 ? "true" : undefined}
          aria-expanded={expanded}
          title={foldTitle}
        >
          {expanded ? (
            <ChevronDown className="icon-sm reasoning-chevron reasoning-chevron-open" />
          ) : (
            <ChevronRight className="icon-sm reasoning-chevron" />
          )}
          {/* 2026-10-01 第二轮：这一行**只说两件事 —— 工作完成 + 共耗时。
              原先还会挂「N 个失败 / N 个跳过」并把失败时的图标换成红色
              告警。撤掉的理由：折叠头是每轮都出现的高频元素，一旦某一轮
              有失败，它就变成一整条红字，于是**每一轮看上去都像出了问题**，
              真正需要注意的那一轮反而淹没在里面。失败在展开后的过程行里
              一眼可见（错误信息本身就是 error 色），那才是它该待的地方。

              这是一次有代价的取舍：折叠态不再提示「这轮有过失败」。
              若日后需要，折中方案是恢复**中性色**的形状图标（不带计数、
              不染红），而不是恢复红字。 */}
          <span className="reasoning-label">
            工作完成{durationPart ? ` · 共耗时 ${durationPart}` : ""}
          </span>
        </button>
      )}
      {/* Chronological render — 一轮就是一个按时间顺序平铺的过程流：
            - text step 永远在自己的时间位置上（这是给用户看的正文）
            - tools step 平铺成过程行；运行中一直可见，结束后随整轮收进折叠头
            - reasoning step 各自是独立折叠项：正在想的那段展开，刚结束的收起
          折叠的唯一总开关是上面的 `tl-process-header`，只在轮次结束后出现。 */}
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
            // 轮次结束且总折叠头收起 → 整轮过程都不渲染。
            // 运行中永远渲染（那时没有总折叠头）；每段自己管自己的开合。
            if (settled && !expanded) return null;
            // 「正在想」= 本段是最后一个 step（轮次尾巴）且仍在流。
            // 下一段思考或工具一追加，active 就变 false → 自动收起。
            const isTail = i === steps.length - 1;
            return (
              <ReasoningBlock
                key={`ps-${i}`}
                content={step.content}
                active={isStreaming && isTail}
                startedAt={step.startedAt}
                endedAt={step.endedAt}
              />
            );
          }
          if (step.kind === "tools" && stepGroups[i]) {
            const groups = stepGroups[i]!;
            if (groups.length === 0) return null;
            // 运行中平铺可见（用户要看着 agent 干活）；结束后随整轮收进折叠头。
            if (settled && !expanded) return null;
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
/**
 * 恢复态的一行过程（DB 的 `role:"tool"` 行）。同样只负责映射数据形状，
 * 渲染委托给 `<ProcessLine>` —— 与运行态的 `ToolGroupCard` 共用同一份
 * JSX。两条路径的差异到此为止。
 */
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
  const vm: ProcessLineVM = {
    tool: meta.tool,
    args: meta.args,
    output: meta.output,
    status: (meta.status ?? "done") as ProcessStatus,
    statusNote: meta.errorReason,
    memoryResult: parseMemoryResult(meta.output),
  };
  return <ProcessLine vm={vm} onPathClick={onPathClick} />;
}
export function StatusPill({ status }: { status: "running" | "done" | "error" | "skipped" | undefined }) {
  // Defense-in-depth: any undefined/garbage status is treated as a
  // completed tool call. The historical bug was that wire-loaded
  // `tool_calls` arrived without a `status` field (Rust's `ToolCall`
  // struct doesn't carry one) and StatusPill's ternary fell through
  // to "运行中". Even with the projection in `api.ts#getSessionMessages`,
  // some legacy code paths still pass `tc.status` straight through —
  // this guard makes the whole component fail-safe.
  const safe: "running" | "done" | "error" | "skipped" =
    status === "running" || status === "done" || status === "error" || status === "skipped"
      ? status
      : "done";
  const cls =
    safe === "done" ? "pill pill-success" :
    safe === "error" ? "pill pill-error" :
    safe === "skipped" ? "pill pill-muted" :
    "pill pill-warning";
  const label =
    safe === "done" ? "完成" :
    safe === "error" ? "失败" :
    safe === "skipped" ? "已跳过" :
    "运行中";
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

