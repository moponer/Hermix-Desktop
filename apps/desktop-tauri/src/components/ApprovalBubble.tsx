// Approval card (role="approval") — extracted from MessageBubble.tsx.
import { AlertCircle, Check, Clock, X } from "lucide-react";
import type { ChatMessage } from "../types";
import { describeToolArgs } from "./bubbleUtils";
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
  const isPending = !meta.decision;
  const approved = meta.decision === "approved";
  const isExpired = meta.decision === "expired";
  const decided = !isPending;
  const desc = describeToolArgs(meta.tool, meta.args);
  // LLM-supplied intent: prefer explicit `meta.reason` (server-stored
  // terminal field), fall back to common LLM habits of writing
  // `reason` / `purpose` / `intent` / `note` inside the tool's args.
  // We display whichever exists so the user sees the actual "why"
  // (e.g. "为新建的 sub-app 创建 manifest") instead of a generic
  // shell-exec boilerplate. Backend schema (ApprovalMessageMeta) does
  // NOT carry reason yet — when the agent-server learns to surface it,
  // the existing `meta.reason` branch wins and args fallback is unused.
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

  const copyCommand = () => {
    const text = meta.args && typeof meta.args === "object"
      ? ((meta.args as Record<string, unknown>).command as string | undefined) ?? desc
      : desc;
    if (typeof navigator !== "undefined" && navigator.clipboard && text) {
      navigator.clipboard.writeText(text).catch(() => {});
    }
  };

  return (
    <div className={`approval-card footer-block h-enter ${riskClass} ${cardStateClass}${decided ? " is-resolved" : ""}`}>
      {/* Body */}
      <div className="approval-body">
        {/* LLM-supplied rationale (terminal schema `reason`, or args
            fallback for `reason`/`purpose`/`intent`/`note`/`rationale`). */}
        {intent ? (
          <div className="approval-reason">
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><path d="M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20z"/><path d="M12 16v-4"/><path d="M12 8h.01"/></svg>
            <span>{intent}</span>
          </div>
        ) : null}

        {/* Command (full text, never truncated) */}
        <div className="approval-cmd-bar">
          <pre className="approval-cmd">{desc}</pre>
          <button type="button" className="approval-copy" onClick={copyCommand} title="复制完整内容">
            复制
          </button>
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
