// components/ApprovalDrawer.tsx
//
// 审批抽屉 —— 从输入框上方弹出的"待决定"面板。
//
// 为什么从消息流里搬出来：审批本质是「现在需要你做的决定」，不是历史
// 记录。内联卡片把它埋在流中间（折叠状态下还会被完全藏住），而抽屉
// 常驻在视线落点（输入框上方）——agent 被阻塞的位置和用户操作的位置
// 是同一个位置。
//
// 职责边界：
// - 抽屉只处理**待决定**的审批；已决定的历史卡片仍由 ProcessTimeline
//   内联渲染（那是记录，属于流）。
// - 批量审批（approval_request_batch）与单个审批共用抽屉，一次显示
//   最老的一项（agent 正阻塞在哪一项，就显示哪一项），右上角计数。
// - 快捷键仅在焦点不在输入类元素时生效 —— 输入框里打 "y" 绝不能
//   变成"允许"，宁可让用户多点一下。

import { useEffect, useState } from "react";
import { AlertTriangle, ChevronDown, ChevronUp, ShieldCheck } from "lucide-react";
import type { ApprovalBatchMessageMeta, ApprovalMessageMeta } from "../types";
import { describeToolArgs, toolVerb } from "./bubbleUtils";

export interface PendingApprovals {
  singles: ApprovalMessageMeta[];
  batches: ApprovalBatchMessageMeta[];
}

interface ApprovalDrawerProps {
  pending: PendingApprovals;
  onApprove: (requestId: string, remember?: boolean) => void;
  onDeny: (requestId: string) => void;
  onApproveBatch: (batchId: string, remember?: boolean) => void;
  onDenyBatch: (batchId: string) => void;
}

type Risk = "dangerous" | "caution" | "safe";
const RISK_LABEL: Record<Risk, string> = {
  dangerous: "高风险", caution: "中风险", safe: "低风险",
};

function riskKey(level?: string): Risk {
  const k = (level ?? "caution").toLowerCase();
  return k === "dangerous" || k === "caution" || k === "safe" ? (k as Risk) : "caution";
}

function extractPath(args: unknown): string | null {
  if (!args || typeof args !== "object") return null;
  const a = args as Record<string, unknown>;
  for (const k of ["path", "file", "filePath", "target"]) {
    const v = a[k];
    if (typeof v === "string" && v.trim()) return v.trim();
  }
  return null;
}

function totalPending(p: PendingApprovals): number {
  return p.singles.length + p.batches.length;
}

export function hasDrawerContent(p: PendingApprovals | null): boolean {
  return !!p && totalPending(p) > 0;
}

export function ApprovalDrawer({
  pending,
  onApprove,
  onDeny,
  onApproveBatch,
  onDenyBatch,
}: ApprovalDrawerProps) {
  const total = totalPending(pending);
  const single = pending.singles[0] ?? null;
  const batch = !single ? (pending.batches[0] ?? null) : null;

  const [showDiff, setShowDiff] = useState(false);
  useEffect(() => {
    setShowDiff(false);
  }, [single?.requestId, batch?.batchId]);

  // 快捷键：⏎/Y=允许一次 · A=会话内允许 · N=拒绝。焦点在输入类元素
  // 里时不生效 —— 打字打出 "y" 变成"允许"是绝对不可接受的事故。
  useEffect(() => {
    if (total === 0) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      const tag = t?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || t?.isContentEditable) return;
      const k = e.key.toLowerCase();
      const remember = true;
      if (k === "enter" || k === "y") {
        if (single) onApprove(single.requestId, false);
        else if (batch) onApproveBatch(batch.batchId, false);
      } else if (k === "a") {
        if (single) onApprove(single.requestId, remember);
        else if (batch) onApproveBatch(batch.batchId, remember);
      } else if (k === "n") {
        if (single) onDeny(single.requestId);
        else if (batch) onDenyBatch(batch.batchId);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [total, single, batch, onApprove, onDeny, onApproveBatch, onDenyBatch]);

  if (total === 0) return null;

  const risk = single ? riskKey(single.riskLevel) : (batch ? "caution" : "caution");
  const riskCls =
    risk === "dangerous" ? "text-error bg-error-soft"
    : risk === "safe" ? "text-success bg-success-soft"
    : "text-warning bg-warning-soft";

  const diff = single?.diff ?? null;
  const desc = single ? describeToolArgs(single.tool, single.args) : "";
  const path = single ? extractPath(single.args) : null;
  const verb = single ? toolVerb(single.tool, JSON.stringify(single.args ?? "")) : "执行操作";

  return (
    <div className="approval-drawer" role="alertdialog" aria-label="审批请求">
      {/* 头：风险 + 工具身份 + 队列计数 */}
      <div className="approval-drawer__head">
        <span className={`approval-drawer__risk ${riskCls}`}>
          {risk === "safe" ? <ShieldCheck className="icon-sm" /> : <AlertTriangle className="icon-sm" />}
          {RISK_LABEL[risk]}
        </span>
        <span className="approval-drawer__tool">
          {single ? `${verb} · ${single.tool}` : `批量审批 · ${batch?.items.length ?? 0} 个操作`}
        </span>
        <span className="approval-drawer__count">
          {total > 1 ? `1 / ${total}` : ""}
        </span>
      </div>

      {/* 体：一句话说明 + 目标预览（+ 可折叠 diff） */}
      <div className="approval-drawer__body">
        {single && (
          <>
            <div className="approval-drawer__desc">
              {single.reason ? single.reason : desc}
            </div>
            {path && (
              <code className="approval-drawer__path" title={path}>{path}</code>
            )}
            {!path && (
              <code className="approval-drawer__path" title={desc}>{desc}</code>
            )}
            {diff && (
              <div className="approval-drawer__diff">
                <button
                  type="button"
                  className="approval-drawer__diff-toggle"
                  onClick={() => setShowDiff((v) => !v)}
                  aria-expanded={showDiff}
                >
                  {showDiff ? <ChevronUp className="icon-sm" /> : <ChevronDown className="icon-sm" />}
                  查看改动
                </button>
                {showDiff && <pre className="approval-drawer__diff-pre">{diff}</pre>}
              </div>
            )}
          </>
        )}
        {batch && (
          <ul className="approval-drawer__items">
            {batch.items.slice(0, 3).map((it) => (
              <li key={it.requestId ?? it.tool}>
                <span className="approval-drawer__item-tool">{it.tool}</span>
                <span className="approval-drawer__item-desc">
                  {describeToolArgs(it.tool, it.args)}
                </span>
              </li>
            ))}
            {batch.items.length > 3 && (
              <li className="approval-drawer__more">…等 {batch.items.length} 个操作</li>
            )}
          </ul>
        )}
      </div>

      {/* 脚：三选项。主按钮在最右（允许一次 = 最常见的决定）。 */}
      <div className="approval-drawer__actions">
        {single ? (
          <>
            <button type="button" className="approval-drawer__btn approval-drawer__btn--ghost"
              onClick={() => onDeny(single.requestId)}>
              拒绝<kbd>N</kbd>
            </button>
            <button type="button" className="approval-drawer__btn approval-drawer__btn--ghost"
              onClick={() => onApprove(single.requestId, true)}>
              会话内允许<kbd>A</kbd>
            </button>
            <button type="button" className="approval-drawer__btn approval-drawer__btn--primary"
              onClick={() => onApprove(single.requestId, false)}>
              允许一次<kbd>⏎</kbd>
            </button>
          </>
        ) : batch ? (
          <>
            <button type="button" className="approval-drawer__btn approval-drawer__btn--ghost"
              onClick={() => onDenyBatch(batch.batchId)}>
              全部拒绝<kbd>N</kbd>
            </button>
            <button type="button" className="approval-drawer__btn approval-drawer__btn--ghost"
              onClick={() => onApproveBatch(batch.batchId, true)}>
              会话内允许<kbd>A</kbd>
            </button>
            <button type="button" className="approval-drawer__btn approval-drawer__btn--primary"
              onClick={() => onApproveBatch(batch.batchId, false)}>
              允许<kbd>⏎</kbd>
            </button>
          </>
        ) : null}
      </div>
    </div>
  );
}
