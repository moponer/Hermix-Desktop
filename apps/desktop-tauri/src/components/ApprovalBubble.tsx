// Approval card rendered for `role="approval"` legacy messages AND
// for the inline `kind: "approval_request"` timeline step emitted
// from the assistant's `unified_steps` (the new chronological path
// — see `types.ts::TimelineStep`).
import { AlertCircle, Check, Clock, X } from "lucide-react";
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

  // Card state class — drives the resolved-state tint only.
  const cardStateClass = decided
    ? (approved ? "approval-card--ok" : isExpired ? "approval-card--expired" : "approval-card--deny")
    : "";

  // Risk tint class — drives a subtle border/wash tint on the whole card.
  // Dangerous → red wash, caution → amber wash, anything else → neutral.
  const riskClass = (meta.riskLevel ?? "").toLowerCase() === "dangerous"
    ? "is-danger"
    : (meta.riskLevel ?? "").toLowerCase() === "caution"
      ? "is-warn"
      : "";

  return (
    <div className={`approval-card footer-block h-enter ${riskClass} ${cardStateClass}${decided ? " is-resolved" : ""}`}>
      {/* 标题栏 — [工具名] + reason(LLM 解释的意图).
          主人要求"reason 行做标题栏, 移除等待你的决定".
          only pending + 有 reason 时显示; 否则跳过此节. */}
      {isPending && (
        <div className="approval-head">
          <span className="approval-head-tool">{meta.tool}</span>
          {intent && <span className="approval-head-reason">{intent}</span>}
        </div>
      )}

      {/* Body */}
      <div className="approval-body">
        {/* reason 上移到标题栏后, body 不再重复渲染 */}

        {/* Command (full text, never truncated) */}
        <div className="approval-cmd-bar">
          <pre className="approval-cmd">{desc}</pre>
        </div>

        {/* Generic risk note — only when no intent is available; if
            the LLM supplied `reason`/`purpose`/etc. we trust the
            specific intent over boilerplate warning copy. */}
        {!intent ? (
          <p className="approval-note">执行任意命令，可读写文件系统。仅在你确认命令可信时允许。</p>
        ) : null}

        {/* Pending: actions only */}
        {isPending ? (
          <div className="approval-foot">
            <div className="approval-btns">
              <button
                type="button"
                onClick={() => onDeny?.(meta.requestId)}
                className="approval-btn-deny"
              >
                拒绝
              </button>
              <button
                type="button"
                onClick={() => onApprove?.(meta.requestId)}
                className="approval-btn-allow"
              >
                允许
              </button>
            </div>
          </div>
        ) : null}
      </div>

      {/* Collapsed one-line summary (resolved state) — tool | desc | status | result */}
      {decided ? (
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
      ) : null}
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
