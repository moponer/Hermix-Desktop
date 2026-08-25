// Approval card rendered for `role="approval"` legacy messages AND
// for the inline `kind: "approval_request"` timeline step emitted
// from the assistant's `unified_steps` (the new chronological path
// — see `types.ts::TimelineStep`).
import {
  AlertCircle,
  Check,
  Clock,
  Copy,
  FileText,
  Globe,
  ShieldAlert,
  Terminal,
  Wrench,
  X,
} from "lucide-react";
import { useState } from "react";
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
 * 视觉层次 (三级):
 *   ① 头部 — 身份层: 需要确认 + 风险等级徽章
 *   ② 主体 — 工具身份 → 意图 → 将执行的命令
 *   ③ 底部 — 决策区: 拒绝 / 允许, 主从分明
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
  const [copied, setCopied] = useState(false);

  // Risk label + badge tint class.
  const riskKey = (meta.riskLevel ?? "dangerous").toLowerCase();
  const riskLabel =
    riskKey === "dangerous" ? "高危"
    : riskKey === "caution" ? "中危"
    : "低危";
  const riskBadgeCls =
    riskKey === "dangerous" ? "is-danger"
    : riskKey === "caution" ? "is-warn"
    : "is-safe";
  // Container tint class — drives the header wash only.
  const riskClass =
    riskKey === "dangerous" ? "is-danger"
    : riskKey === "caution" ? "is-warn"
    : "";
  const ToolIco = toolIcon(meta.tool);

  const handleCopy = () => {
    try {
      void navigator.clipboard?.writeText(desc);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard unavailable — ignore */
    }
  };

  return (
    <div className={`approval-card footer-block h-enter ${riskClass}${decided ? " is-resolved" : ""}`}>
      {/* Collapsible pending body — shrinks to the one-line summary
          when the card flips to `.is-resolved`. */}
      <div className="approval-full">
        {/* ① 头部: 身份层 */}
        <div className="apv-head">
          <span className="apv-head-icon">
            <ShieldAlert size={15} />
          </span>
          <div className="apv-head-text">
            <span className="apv-head-title">需要你确认</span>
            <span className="apv-head-sub">AI 助手请求执行以下操作</span>
          </div>
          <span className={`apv-risk ${riskBadgeCls}`}>{riskLabel}</span>
        </div>

        {/* ② 主体 */}
        <div className="apv-body">
          {/* 工具身份行 */}
          <div className="apv-tool">
            <span className="apv-tool-icon">
              <ToolIco size={13} />
            </span>
            <span className="apv-tool-name">{meta.tool}</span>
            <span className="apv-tool-tag">工具调用</span>
          </div>

          {/* 意图 callout — LLM 解释的意图, 决策前先读意图 */}
          {intent && (
            <div className="apv-intent">
              <span className="apv-intent-label">意图</span>
              <span className="apv-intent-text">{intent}</span>
            </div>
          )}

          {/* 命令块 — 将执行的动作, 可复制 */}
          <div className="apv-cmd">
            <div className="apv-cmd-head">
              <span className="apv-cmd-label">将执行</span>
              <button
                type="button"
                className="apv-copy"
                onClick={handleCopy}
                aria-label="复制命令"
              >
                <Copy size={11} />
                {copied ? "已复制" : "复制"}
              </button>
            </div>
            <pre className="apv-cmd-pre">{desc}</pre>
          </div>

          {/* ③ 底部决策区 — 拒绝 / 允许, 主从分明 */}
          {isPending && (
            <div className="apv-foot">
              <span className="apv-foot-hint">拒绝将中止本次操作</span>
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
            </div>
          )}
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
