// Approval card rendered for `role="approval"` legacy messages AND
// for the inline `kind: "approval_request"` timeline step emitted
// from the assistant's `unified_steps` (the new chronological path
// — see `types.ts::TimelineStep`).
import {
  AlertCircle,
  Check,
  Clock,
  FileText,
  Globe,
  Terminal,
  Wrench,
  X,
} from "lucide-react";
import type { ApprovalMessageMeta, ChatMessage } from "../types";
import { describeToolArgs } from "./bubbleUtils";

/**
 * Bottom-of-card actions. Pending state shows "拒绝 / 允许";
 * resolved state is rendered as a one-line summary below the body.
 */
export interface ApprovalRequestViewProps {
  meta: ApprovalMessageMeta;
  onApprove?: (requestId: string) => void;
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

/**
 * Pure renderer: takes an `ApprovalMessageMeta` directly. Used by
 * `ApprovalCard` (legacy `role: "approval"` path) and by
 * `ToolCallBubble::ProcessTimeline` for the inline `kind:
 * "approval_request"` timeline step. Drives both the pending
 * "拒绝 / 允许" form and the resolved 1-line summary.
 *
 * 严格两行布局 (主人反馈"看起来很乱"后砍级):
 *   ① 第 1 行 — 信息 + 决策: 工具图标/名 + 意图摘要(左对齐),
 *                拒绝 / 允许 按钮(最右侧)
 *   ② 第 2 行 — 命令块: 直接展示命令文本 (标题栏/复制已移除)
 * 风险等级通过工具图标染色 (高危红 / 中危橙 / 低危默认) 隐式传达.
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

  // Risk class — drives the tool-icon tint.
  // 高危红 / 中危橙 / 低危默认色.
  const riskKey = (meta.riskLevel ?? "dangerous").toLowerCase();
  const riskBadgeCls =
    riskKey === "dangerous" ? "is-danger"
    : riskKey === "caution" ? "is-warn"
    : "is-safe";
  const ToolIco = toolIcon(meta.tool);

  return (
    <div className={`approval-card footer-block h-enter${decided ? " is-resolved" : ""}`}>
      {/* Collapsible pending body — shrinks to the one-line summary
          when the card flips to `.is-resolved`. */}
      <div className="approval-full">
        {/* 严格两行布局:
            第 1 行: 信息 + 决策 — 工具图标/名 + 意图摘要(左对齐),
                     拒绝/允许 按钮(最右侧)
            第 2 行: 命令块 — 标题栏"将执行"+ 复制按钮 + 命令文本
            风险等级通过工具图标染色隐式传达 (高危红/中危橙/低危默认),
            不再单独显示徽章. */}

        {/* ① 第 1 行: 信息 + 决策 */}
        <div className="apv-decision-row">
          <div className="apv-info">
            <span className={`apv-tool-icon ${riskBadgeCls}`}>
              <ToolIco size={13} />
            </span>
            <span className="apv-tool-name">{meta.tool}</span>
            <span className="apv-sep">·</span>
            <span className="apv-intent-text">{intent ?? "执行工具调用"}</span>
          </div>
          {isPending && (
            <div className="apv-btns">
              <button
                type="button"
                className="apv-deny"
                onClick={() => onDeny?.(meta.requestId)}
              >
                拒绝
              </button>
              <button
                type="button"
                className="apv-allow"
                onClick={() => onApprove?.(meta.requestId)}
              >
                允许
              </button>
            </div>
          )}
        </div>

        {/* ② 第 2 行: 命令块 — 直接展示命令文本 */}
        <div className="apv-cmd">
          <pre className="apv-cmd-pre">{desc}</pre>
        </div>
      </div>

      {/* Collapsed one-line summary (resolved state) — tool | desc | status | result */}
      {decided && (
        <div className="approval-summary">
          <span className={`approval-summary-icon approval-summary-icon-${approved ? "ok" : isExpired ? "expired" : "err"}`}>
            {approved
              ? <Check size={12} />
              : isExpired
                ? <Clock size={12} />
                : <X size={12} />}
          </span>
          <span className="approval-summary-tool">{meta.tool}</span>
          <span className="approval-summary-desc">{desc}</span>
          <span className={`approval-summary-badge approval-summary-badge-${approved ? "ok" : isExpired ? "expired" : "err"}`}>
            {approved ? "你已允许" : isExpired ? "已超时" : "你已拒绝"}
          </span>
          <span className="approval-summary-result">
            {approved
              ? "你已批准，操作继续执行"
              : isExpired
                ? "超时未处理，操作已自动取消"
                : "你已拒绝，操作已中止"}
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
  onApprove?: (requestId: string) => void;
  onDeny?: (requestId: string) => void;
}) {
  const meta = message.approval;
  if (!meta) {
    return (
      <div className="h-enter flex items-center gap-2 text-[12px] text-text-faint italic px-2 py-1.5">
        <AlertCircle size={12} />
        <span>权限请求数据缺失</span>
      </div>
    );
  }
  return (
    <ApprovalRequestView meta={meta} onApprove={onApprove} onDeny={onDeny} />
  );
}
