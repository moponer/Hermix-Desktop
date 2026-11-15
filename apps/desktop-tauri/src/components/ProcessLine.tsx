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
import { COMPLETED } from "./strings";

/** 与 `messageParser.ToolGroup.status` / `types.ToolMessageMeta.status` 同形。 */
export type ProcessStatus = "running" | "done" | "error" | "skipped";

/**
 * 「当前目录」的相对记号。目录类工具（`list_files` / `list_directory` /
 * `glob` …）在没指定路径时一律传 `path: "."` —— 对工具是对的位置，
 * 对读行的人是零信息量。命中这组记号且拿得到工作区根目录时，primary 槽
 * 改显示根目录本身。
 *
 * 刻意**不**把「所有相对路径」都展开成绝对路径：那会让
 * `apps/desktop-tauri/src` 变成一长串盘符路径，既挤爆 `max-width: 46%`
 * 的省略号，又和参考界面的相对写法不一致。相对但有信息量的照旧原样显示。
 */
const CWD_MARKERS = new Set([".", "./", "..", "../", "", "\\\\"]);

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
  /**
   * 工作区根目录。只用来展开「当前目录」记号（见 `CWD_MARKERS`）——
   * 目录类工具最爱传 `path: "."`，那是个指针不是路径。
   */
  workspaceRoot?: string | null;
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
  done: COMPLETED,
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

  // 结果摘要：成功/运行中显示结果首行；失败/跳过显示**原因**。
  // 2026-10-02 用户实机反馈：失败的 `查看目录` 整行只有一个「.」，
  // 原因一个字都没有 —— 原因链上有两处会**同时**返回 null：
  //   1. `summarizeError` 遇到 `{"result": "..."}` 这种不是错误形状的
  //      wrapper 时主动返回 null（它不肯把 result 当错误）；
  //   2. `toolResultPeek` 剥完 wrapper 后若首行仍是 JSON，也返回 null。
  // 两条都 null 时这一行就哑了。取用顺序：`statusNote`（判定时已经算好的
  // 结构化原因）→ `summarizeError`（剥 JSON 错误体）→ `toolResultPeek`
  // （剥普通 wrapper）。宁可给一行短的，也不能给一行没有原因的错误。
  const reason = useMemo(() => {
    if (!isError && !isSkipped) return null;
    return (
      vm.statusNote ??
      summarizeError(vm.output) ??
      toolResultPeek(vm.output, vm.args)
    );
  }, [vm.output, vm.args, vm.statusNote, isError, isSkipped]);

  const peek = useMemo(() => {
    if (isError || isSkipped) return reason;
    if (!vm.output || vm.output.trim().length === 0) return null;
    if (isRunning) return null;
    return toolResultPeek(vm.output, vm.args);
  }, [vm.output, vm.args, isError, isSkipped, reason, isRunning]);

  const Icon = toolIcon(vm.tool);
  // memory 工具的动词来自结构化结果（编辑记忆 / 新增记忆 …）
  const verb = mem ? memoryActionLabel(mem.action) : toolVerb(vm.tool, vm.args);

  // primary 槽 = ** acted on 的是什么**，不再被失败原因占用。
  // 2026-10-02：原来错误时 primary 优先取 `summarizeError`，把「对什么
  // 动手」挤掉了 —— 一行失败的工具调用就变成了「查看目录 <错误原因>」，
  // 用户既不知道读的是哪个目录，也不知道失败的是哪一步。两件事各占一槽：
  // primary 给对象，peek 给结果/原因。
  const rawPrimary = mem ? mem.path : toolPrimaryArg(vm.tool, vm.args, vm.output);
  const isPathLike = mem ? true : isPathArg(vm.tool, vm.args, vm.output);
  // `.` / `./` / `..` 是「当前目录」的相对记号：它对读行的人**零信息量**
  // ——「查看目录 .」不告诉任何人这是哪个目录。目录类工具恰恰最爱这么传。
  // 判据是「这个记号指向哪里」而不是「它是不是相对路径」：相对但有信息量
  // 的 `apps/desktop-tauri/src` 原样显示（和参考界面一致），只有纯记号才
  // 用工作区根目录顶替。
  const isCwdMarker =
    isPathLike && !mem && CWD_MARKERS.has((rawPrimary ?? "").trim());
  const primary =
    isCwdMarker && vm.workspaceRoot ? vm.workspaceRoot : rawPrimary;
  const stats = extractLineStats(vm.tool, vm.args);
  const clickablePath = mem
    ? mem.path
    : isCwdMarker && vm.workspaceRoot
      ? vm.workspaceRoot
      : isPathLike
        ? (vm.output || vm.args)
        : null;
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
        {peek && (
          <span
            className={
              isError
                ? "process-line-peek process-line-peek--error"
                : "process-line-peek"
            }
          >
            {peek}
          </span>
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
      {/* 2026-10-01：原来失败/跳过的原因会在下面**再开一行**重复一遍。
          删掉 —— 原因已经在上面那一行的 peek 槽里了（错误时 peek 装的是
          reason），而完整输出点 chevron 就能展开。两行说同一件事，纯噪音。 */}
      {open && hasOutput && (
        <div className="process-line-body">
          <LiveTerminal content={vm.output ?? ""} running={isRunning} />
        </div>
      )}
    </div>
  );
}
