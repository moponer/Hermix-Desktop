// Aggregate approval card — server coalesces N parallel high-risk
// tool calls into a single `RequestApprovalsBatch` envelope; this
// component renders one card with a list of sub-actions and three
// buttons (拒绝 / 允许 / 以后自动允许) that resolve the batch with
// one decision applied uniformly. The lifecycle mirrors
// `ApprovalRequestView` (pending / resolved / remembered), so the
// surrounding `ProcessTimeline` step-fold and the
// `.tl-approval--leaving` collapse animation behave identically.
import { Check, Clock, Layers, X } from "lucide-react";
import type { ApprovalBatchMessageMeta } from "../types";
import { describeToolArgs } from "./bubbleUtils";

export interface ApprovalBatchViewProps {
  meta: ApprovalBatchMessageMeta;
  onApprove?: (batchId: string, remember?: boolean) => void;
  onDeny?: (batchId: string) => void;
}

export function ApprovalBatchView({
  meta,
  onApprove,
  onDeny,
}: ApprovalBatchViewProps) {
  const isPending = !meta.decision;
  const approved = meta.decision === "approved";
  const isExpired = meta.decision === "expired";
  const decided = !isPending;
  const count = meta.items.length;
  // Tool composition: surface the dominant tool for the head pill;
  // the item list below shows each tool individually.
  const toolCounts = meta.items.reduce<Record<string, number>>((acc, it) => {
    acc[it.tool] = (acc[it.tool] ?? 0) + 1;
    return acc;
  }, {});
  const dominantTool =
    Object.entries(toolCounts).sort((a, b) => b[1] - a[1])[0]?.[0] ?? "tool";

  return (
    <div className={`approval-card footer-block h-enter${decided ? " is-resolved" : ""}`}>
      {/* Pending body — list of items + 3 batch buttons. */}
      <div className="approval-full">
        <div className="apv-decision-row">
          <div className="apv-info">
            <span className="apv-tool-icon is-warn">
              <Layers size={13} />
            </span>
            <span className="apv-tool-name">
              {count} 个{dominantTool === "tool" ? "" : ` ${dominantTool}`}调用
            </span>
            <span className="apv-sep">·</span>
            <span className="apv-intent-text">等待批量审批</span>
          </div>
          {isPending && (
            <div className="apv-btns">
              <button
                type="button"
                className="apv-deny"
                onClick={() => onDeny?.(meta.batchId)}
              >
                拒绝
              </button>
              <button
                type="button"
                className="apv-allow"
                onClick={() => onApprove?.(meta.batchId)}
              >
                允许
              </button>
              <button
                type="button"
                className="apv-always"
                title="相同命令之后将自动同意，不再询问"
                onClick={() => onApprove?.(meta.batchId, true)}
              >
                以后自动允许
              </button>
            </div>
          )}
        </div>
        {/* Compact sub-action list — mirrors the single-card body but
            shows every item as a one-line summary, no per-item
            buttons (decision applies uniformly). */}
        <ul className="apv-batch-list">
          {meta.items.map((it) => {
            const intent =
              (typeof it.reason === "string" && it.reason) || null;
            const desc = describeToolArgs(it.tool, it.args);
            return (
              <li key={it.requestId} className="apv-batch-item">
                <span className={`apv-batch-tool`}>{it.tool}</span>
                <span className="apv-batch-sep">·</span>
                <span className="apv-batch-intent">{intent ?? desc ?? "执行工具调用"}</span>
              </li>
            );
          })}
        </ul>
      </div>

      {/* Collapsed one-line summary when decided. */}
      {decided && (
        <div className="approval-summary">
          <span
            className={`approval-summary-icon approval-summary-icon-${
              approved ? "ok" : isExpired ? "expired" : "err"
            }`}
          >
            {approved
              ? <Check size={12} />
              : isExpired
                ? <Clock size={12} />
                : <X size={12} />}
          </span>
          <span className="approval-summary-tool">{count} 个调用</span>
          <span className="approval-summary-sep">·</span>
          <span className="approval-summary-desc">批量决策</span>
          {/* 右侧状态 — flex push to end */}
          <span className="approval-summary-status">
            {approved
              ? meta.remembered
                ? "已批准 · 同款自动通过"
                : "已批准 · 继续执行"
              : isExpired
                ? "已超时 · 自动取消"
                : "已拒绝 · 已中止"}
          </span>
        </div>
      )}
    </div>
  );
}

// `dominantTool` is computed inline above from the items list.