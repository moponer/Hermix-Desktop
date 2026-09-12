// Approval card rendered for `role="approval"` legacy messages AND
// for the inline `kind: "approval_request"` timeline step emitted
// from the assistant's `unified_steps` (the new chronological path
// — see `types.ts::TimelineStep`).
//
// Modern 5-layer info architecture (2026-08-30, 同步 ApprovalBatchBubble):
//   ① Header:  工具图标 + "工具调用" label + 风险 chip
//   ② Verb:    动词 (toolVerb) + 目标 (文件名) + 意图摘要
//   ③ File:    完整文件路径 + 影响范围
//   ④ Detail:  折叠面板 — 当前显示 describeToolArgs 描述（diff 数据接入后这里替换）
//   ⑤ Actions: 快捷键 footer (Y/N/A) + 决策按钮 (拒绝 / 允许)
//
// Keyboard shortcuts:  Y = 允许  N = 拒绝  A = 以后自动允许
// 快捷键 listener 过滤 INPUT/TEXTAREA + modifier keys, 避免与文本编辑冲突.
// 风险 chip 用后端 `meta.riskLevel`; batch 卡改用 `items[].risk`(服务端工具注册表判定),
// 不再按工具名猜测.
import { useEffect, useState } from "react";
import {
  AlertCircle,
  Check,
  ChevronDown,
  ChevronRight,
  Clock,
  FileText,
  Globe,
  Terminal,
  Wrench,
  X,
} from "lucide-react";
import type { ApprovalMessageMeta, ChatMessage } from "../types";
import { describeToolArgs, toolVerb } from "./bubbleUtils";

/**
 * Bottom-of-card actions. Pending state shows "拒绝 / 允许";
 * resolved state is rendered as a one-line summary below the body.
 */
export interface ApprovalRequestViewProps {
  meta: ApprovalMessageMeta;
  /** `remember=true` carries the「以后自动允许」intent through to the
   *  server-side allowlist; defaults to false (one-shot allow). */
  onApprove?: (requestId: string, remember?: boolean) => void;
  onDeny?: (requestId: string) => void;
}

/** Map a tool name to a lucide icon for the "工具身份" row. */
function toolIcon(tool: string) {
  const t = tool.toLowerCase();
  if (t.includes("terminal") || t.includes("shell") || t.includes("exec") || t.includes("bash")) return Terminal;
  if (t.includes("browser") || t.includes("web") || t.includes("http") || t.includes("fetch") || t.includes("url")) return Globe;
  if (t.includes("file") || t.includes("read") || t.includes("write") || t.includes("patch")) return FileText;
  return Wrench;
}

/** Risk level → 中文标签. 默认 dangerous（防御性 — legacy 老数据无 riskLevel 字段时降级为最高风险）. */
type Risk = "dangerous" | "caution" | "safe";
const RISK_LABEL: Record<Risk, string> = {
  dangerous: "高风险", caution: "中风险", safe: "低风险",
};

/** Best-effort file path extraction (与 ApprovalBatchBubble 共享语义).
 *  Many tools store the file path under `path` / `file` / `filePath` / `target`. */
function extractPath(args: unknown): string | null {
  if (!args || typeof args !== "object" || Array.isArray(args)) return null;
  const a = args as Record<string, unknown>;
  for (const k of ["path", "file", "filePath", "target"]) {
    const v = a[k];
    if (typeof v === "string" && v.trim()) return v.trim();
  }
  return null;
}

/** `apv-file-stats` 行：line count（仅 file 工具）+ "当前目录 · 可回滚". */
function fileStats(tool: string, args: unknown): string {
  const parts: string[] = [];
  if (args && typeof args === "object" && !Array.isArray(args)) {
    const a = args as Record<string, unknown>;
    if (typeof a.lineCount === "number" && a.lineCount > 0) parts.push(`${a.lineCount} 行变更`);
  }
  if (tool.toLowerCase().match(/write|edit|patch|file/)) parts.push("当前目录 · 可回滚");
  return parts.join(" · ") || "可回滚";
}

/**
 * Pure renderer: takes an `ApprovalMessageMeta` directly. Used by
 * `ApprovalCard` (legacy `role: "approval"` path) and by
 * `ToolCallBubble::ProcessTimeline` for the inline `kind:
 * "approval_request"` timeline step. Drives both the pending
 * "拒绝 / 允许" form and the resolved 1-line summary.
 */
export function ApprovalRequestView({
  meta,
  onApprove,
  onDeny,
}: ApprovalRequestViewProps) {
  const isPending = !meta.decision;
  const approved = meta.decision === "approved";
  const isExpired = meta.decision === "expired";
  const decided = !isPending;
  const desc = describeToolArgs(meta.tool, meta.args);
  // LLM-supplied intent: prefer explicit `meta.reason` (server-stored
  // terminal field), fall back to common LLM habits of writing
  // `reason` / `purpose` / `intent` / `note` inside the tool's args.
  const argsIntent = (() => {
    if (!meta.args || typeof meta.args !== "object" || Array.isArray(meta.args)) return null;
    const a = meta.args as Record<string, unknown>;
    for (const k of ["reason", "purpose", "intent", "note", "rationale"]) {
      const v = a[k];
      if (typeof v === "string" && v.trim()) return v.trim();
    }
    return null;
  })();
  const intent = meta.reason || argsIntent;

  // Risk chip — 后端传 riskLevel 时直接用, 没传默认 dangerous（防御性）.
  const riskKey = (meta.riskLevel ?? "dangerous").toLowerCase();
  const riskNorm: Risk =
    riskKey === "dangerous" ? "dangerous"
    : riskKey === "caution" ? "caution"
    : riskKey === "safe" ? "safe"
    : "dangerous";
  const riskIconCls =
    riskNorm === "dangerous" ? "is-danger"
    : riskNorm === "caution" ? "is-warn"
    : "is-safe";
  const ToolIco = toolIcon(meta.tool);

  // File path + 影响范围（仅 file/edit/write/patch 类工具有意义）.
  const filePath = extractPath(meta.args);
  const fileName = filePath ? filePath.split(/[\\/]/).pop() : null;

  // Detail panel collapsed by default — 当前显示 desc; diff 数据接入后这里替换.
  const [detailOpen, setDetailOpen] = useState(false);

  // Keyboard shortcuts (Y / N / A). Skip when typing in inputs/modifier pressed.
  useEffect(() => {
    if (!isPending) return;
    const handler = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
      const k = e.key.toLowerCase();
      if (k === "y") { e.preventDefault(); onApprove?.(meta.requestId, false); }
      else if (k === "n") { e.preventDefault(); onDeny?.(meta.requestId); }
      else if (k === "a") { e.preventDefault(); onApprove?.(meta.requestId, true); }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [isPending, meta.requestId, onApprove, onDeny]);

  return (
    <div className={`approval-card footer-block h-enter${decided ? " is-resolved" : ""}`}>
      {/* Pending body — modern 5-layer layout */}
      <div className="approval-full">
        <div className="apv-shell">
          {/* ① Header */}
          <div className="apv-header">
            <div className="apv-header-title">
              <span className={`apv-tool-icon ${riskIconCls}`}>
                <ToolIco className="icon-md" />
              </span>
              <span className="apv-header-label">工具调用</span>
            </div>
            <div className="apv-header-meta">
              <span className={`risk-chip is-${riskNorm}`}>{RISK_LABEL[riskNorm]}</span>
            </div>
          </div>
          {/* ② Verb row */}
          <div className="apv-verb-row">
            <span className="apv-verb">{toolVerb(meta.tool)}</span>
            {fileName && <span className="apv-target">{fileName}</span>}
            {!fileName && intent && <span className="apv-target">{intent}</span>}
          </div>
          {/* ③ File block (when path is known) */}
          {filePath && (
            <div className="apv-file-block">
              <span className="apv-file-path" title={filePath}>{filePath}</span>
              <span className="apv-file-stats">{fileStats(meta.tool, meta.args)}</span>
            </div>
          )}
          {/* ④ Detail toggle + panel (current: desc; future: diff) */}
          <button
            type="button"
            className="apv-diff-toggle"
            onClick={() => setDetailOpen(o => !o)}
            aria-expanded={detailOpen}
          >
            {detailOpen
              ? <ChevronDown className="icon-sm" />
              : <ChevronRight className="icon-sm" />}
            {detailOpen ? "收起详情" : "查看详情"}
          </button>
          {detailOpen && (
            <div className="apv-diff-content">
              {meta.diff
                ? <pre className="apv-cmd-pre apv-diff-pre">{meta.diff}</pre>
                : <pre className="apv-cmd-pre">{desc}</pre>}
            </div>
          )}
          {/* ⑤ Actions — shortcut hint + 2 decision buttons */}
          {isPending && (
            <div className="apv-actions">
              <div className="apv-shortcut">
                <span><kbd>Y</kbd>允许</span>
                <span><kbd>N</kbd>拒绝</span>
                <span><kbd>A</kbd>以后自动</span>
              </div>
              <div className="apv-btns">
                <button
                  type="button"
                  className="apv-btn apv-btn-danger"
                  aria-label="拒绝本次工具调用 (快捷键 N)"
                  onClick={() => onDeny?.(meta.requestId)}
                >
                  拒绝
                </button>
                <button
                  type="button"
                  className="apv-btn apv-btn-primary"
                  aria-label="允许本次工具调用 (快捷键 Y)"
                  onClick={() => onApprove?.(meta.requestId)}
                >
                  允许
                </button>
              </div>
            </div>
          )}
        </div>
      </div>

      {/* Collapsed one-line summary when decided. */}
      {decided && (
        <div className="approval-summary">
          <span className={`approval-summary-icon approval-summary-icon-${approved ? "ok" : isExpired ? "expired" : "err"}`}>
            {approved
              ? <Check className="icon-sm" />
              : isExpired
                ? <Clock className="icon-sm" />
                : <X className="icon-sm" />}
          </span>
          <span className="approval-summary-tool">{meta.tool}</span>
          <span className="approval-summary-sep">·</span>
          <span className="approval-summary-desc">{desc}</span>
          <span className="approval-summary-status">
            {approved
              ? meta.remembered
                ? "已批准 · 自动通过"
                : "已批准 · 继续执行"
              : isExpired
                ? "已超时 · 已取消"
                : "已拒绝 · 已中止"}
          </span>
        </div>
      )}
    </div>
  );
}

/**
 * Legacy wrapper for the standalone `role: "approval"` chat message.
 * New approvals push an inline `kind: "approval_request"` step into
 * the assistant's `unified_steps` (rendered via `ApprovalRequestView`
 * inside `<ProcessTimeline>`), so this card is only seen for older
 * sessions persisted before that migration.
 */
export function ApprovalCard({
  message,
  workspaceRoot: _workspaceRoot,
  onApprove,
  onDeny,
}: {
  message: ChatMessage;
  workspaceRoot?: string | null;
  onApprove?: (requestId: string, remember?: boolean) => void;
  onDeny?: (requestId: string) => void;
}) {
  const meta = message.approval;
  if (!meta) {
    return (
      <div className="h-enter flex items-center gap-2 text-[12px] text-text-faint italic px-2 py-1.5">
        <AlertCircle className="icon-sm" />
        <span>权限请求数据缺失</span>
      </div>
    );
  }
  return (
    <ApprovalRequestView meta={meta} onApprove={onApprove} onDeny={onDeny} />
  );
}