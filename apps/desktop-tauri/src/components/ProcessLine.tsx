// 过程行（L2）——运行态与恢复态**共用**的唯一渲染实现。
//
// 为什么是共享组件而不是各写一份：`ToolGroupCard`（运行态，从
// `unified_steps` 的 `tool_complete` 事件现算）和 `ToolMessageCard`
// （恢复态，从 DB 的 `role:"tool"` 行读）此前是两份近乎重复的 JSX，
// 结构一样、字段来源不同。历史上它们已经因此漂移过一次 —— 三处
// `detectToolError` 各抄一份、全部漏判 agent runtime 的
// "Tool 'x' does not exist"，把失败渲染成「✓已完成」。
//
// 这次的结构约束：两者只负责**把自己的数据形状映射成
// `ProcessLineVM`**，渲染一律走 `<ProcessLine>`。运行态和恢复态
// 因此不可能给出不同的视觉或不同的状态语义 —— 除非有人再把渲染
// 复制出去。
import {
  useMemo, useState,
  type MouseEvent as ReactMouseEvent,
} from "react";
import {
  Check, AlertCircle, AlertTriangle, CircleSlash,
  ChevronRight, ChevronDown, Loader2,
} from "lucide-react";
import { LiveTerminal } from "./LiveTerminal";
import {
  toolIcon, toolVerb, toolPrimaryArg, toolResultPeek,
  summarizeError,
} from "./bubbleUtils";
import {
  isPathArg, extractFirstPath, extractLineStats,
} from "./bubbles/ToolOutputPreviews";
import { parseMemoryResult, memoryActionLabel } from "../utils/memoryToolResult";
import type { MemoryToolResult } from "../utils/memoryToolResult";

/** 与 `messageParser.ToolGroup.status` / `types.ToolMessageMeta.status` 同形。 */
export type ProcessStatus = "running" | "done" | "error" | "skipped";

/**
 * 一行过程所需的全部信息，由调用方从自己的数据形状映射而来。
 * 两条路径的唯一差异就止步于构造这个对象。
 */
export interface ProcessLineVM {
  tool: string;
  args?: string;
  /** 工具结果文本（`tool_complete.content` 或 stdout 拼接）。 */
  output?: string;
  status: ProcessStatus;
  /** 失败 / 跳过的单行原因。 */
  statusNote?: string;
  /** 等审批：把「运行中」换成「等待审批」，别让用户以为卡住了。 */
  awaitingApproval?: boolean;
  /** memory 工具的结构化结果，优先于通用 primary 提取。 */
  memoryResult?: MemoryToolResult | null;
}

/* ── 状态 → 图标形状 ──────────────────────────────────────────
   状态由**图标形状**承载，不只靠颜色 —— 文字标签（已完成/失败/运行中）
   占掉的横向空间比 12px 图标多得多，且色觉障碍用户读不出颜色差异时
   形状仍可辨（无障碍基础项，不削减）。每个状态都带 `title` +
   `aria-label`，屏幕阅读器拿得到完整语义。 */
const STATUS_ICON = {
  done: Check,
  error: AlertCircle,
  running: Loader2,
  skipped: CircleSlash,
} as const;

const STATUS_LABEL: Record<ProcessStatus | "awaiting", string> = {
  done: "已完成",
  error: "失败",
  running: "运行中",
  skipped: "已跳过",
  awaiting: "等待审批",
};

export function ProcessLine({
  vm,
  onPathClick,
}: {
  vm: ProcessLineVM;
  onPathClick?: (path: string, e: ReactMouseEvent) => void;
}) {
  const [open, setOpen] = useState(false);

  const isRunning = vm.status === "running";
  const isError = vm.status === "error";
  const isSkipped = vm.status === "skipped";
  // 调用方可以直接传解析结果；没传就自己兜一层，省得两条路径各写一遍。
  const mem = vm.memoryResult ?? parseMemoryResult(vm.output);

  // 结果摘要：错误/跳过优先显示原因，其余显示结果首行。
  const peek = useMemo(() => {
    if (vm.status === "error") {
      return (
        vm.statusNote ??
        summarizeError(vm.output) ??
        (vm.output ? toolResultPeek(vm.output, vm.args) : null) ??
        "执行出错"
      );
    }
    if (isSkipped) {
      return vm.statusNote ?? "工具未执行";
    }
    if (!vm.output || vm.output.trim().length === 0) return null;
    if (isRunning) return null;
    return toolResultPeek(vm.output, vm.args);
  }, [vm.output, vm.args, vm.status, vm.statusNote, isSkipped, isRunning]);

  const Icon = toolIcon(vm.tool);
  // memory 工具的动词来自结构化结果（编辑记忆 / 新增记忆 …）
  const verb = mem ? memoryActionLabel(mem.action) : toolVerb(vm.tool, vm.args);
  // 错误/跳过的 primary 槽直接给原因，正文槽留给结果摘要
  const primary = mem
    ? mem.path
    : isError || isSkipped
      ? (summarizeError(vm.output) ?? toolPrimaryArg(vm.tool, vm.args, vm.output))
      : toolPrimaryArg(vm.tool, vm.args, vm.output);
  const stats = extractLineStats(vm.tool, vm.args);
  const isPathLike = mem ? true : isPathArg(vm.tool, vm.args, vm.output);
  const clickablePath = mem ? mem.path : isPathLike ? (vm.output || vm.args) : null;
  const hasOutput = vm.output != null && vm.output.length > 0;

  const handlePrimaryClick = (e: ReactMouseEvent) => {
    const p = clickablePath && onPathClick ? extractFirstPath(clickablePath) : null;
    if (p) {
      e.stopPropagation();
      onPathClick!(p, e);
      return;
    }
    if (hasOutput) setOpen((v) => !v);
  };

  const stateKey: ProcessStatus | "awaiting" =
    vm.awaitingApproval && isRunning ? "awaiting" : vm.status;
  const StateIcon =
    stateKey === "awaiting" ? AlertTriangle : STATUS_ICON[vm.status];
  const stateText = STATUS_LABEL[stateKey];

  return (
    <div
      className="process-line"
      data-state={stateKey === "awaiting" ? "awaiting" : vm.status}
      /* 展开态也要露出箭头 —— 否则用户点了展开、行也长高了，
         但唯一的「收回去」线索消失了。 */
      data-open={open ? "true" : "false"}
    >
      <button
        type="button"
        onClick={hasOutput ? () => setOpen((v) => !v) : undefined}
        className="process-line-row"
        aria-expanded={hasOutput ? open : undefined}
        aria-label={`${verb}${primary ? ` ${primary}` : ""} — ${stateText}`}
      >
        <Icon className="process-line-icon" />
        <span className="process-line-verb">{verb}</span>
        {primary && (
          <span
            className={isPathLike ? "process-line-primary process-line-primary--link" : "process-line-primary"}
            title={isPathLike ? `点击打开 ${primary}` : primary}
            onClick={isPathLike ? handlePrimaryClick : undefined}
          >
            {primary}
          </span>
        )}
        {peek && !isError && (
          <span className="process-line-peek">{peek}</span>
        )}
        <span className="process-line-right">
          {(stats.added != null || stats.removed != null) && (
            <span className="process-line-stats">
              {stats.added != null && stats.added > 0 && (
                <span className="process-line-stat process-line-stat--add">+{stats.added}</span>
              )}
              {stats.removed != null && stats.removed > 0 && (
                <span className="process-line-stat process-line-stat--del">−{stats.removed}</span>
              )}
            </span>
          )}
          {/* 状态：形状 + 可访问名称，无视觉文字标签 */}
          <span className="process-line-state" title={stateText}>
            <StateIcon
              className={`process-line-state-icon${isRunning ? " animate-spin" : ""}`}
              aria-hidden="true"
            />
            <span className="sr-only">{stateText}</span>
          </span>
          {hasOutput && (
            <span className="process-line-chev">
              {open ? <ChevronDown /> : <ChevronRight />}
            </span>
          )}
        </span>
      </button>
      {/* 错误/跳过的原因独立成行：不能只靠 title tooltip，
          那是鼠标用户专属的通道。 */}
      {(isError || isSkipped) && vm.statusNote && (
        <div className="process-line-note" onClick={() => setOpen(true)}>
          {isSkipped ? <CircleSlash aria-hidden="true" /> : <AlertCircle aria-hidden="true" />}
          <span className="process-line-note-text">{vm.statusNote}</span>
          {hasOutput && <ChevronRight className="process-line-chev" aria-hidden="true" />}
        </div>
      )}
      {open && hasOutput && (
        <div className="process-line-body">
          <LiveTerminal content={vm.output ?? ""} running={isRunning} />
        </div>
      )}
    </div>
  );
}
