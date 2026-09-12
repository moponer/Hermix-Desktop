// Aggregate approval card — server coalesces N parallel high-risk
// tool calls into a single `RequestApprovalsBatch` envelope; this
// component renders one card with a modern 5-layer info architecture:
//
//   ① Header  — 工具图标 + "工具调用" label + 风险 chip + 项数
//   ② Verb    — 动词 (e.g. "编辑文件") + 目标 (e.g. "index.html")
//   ③ File    — 完整文件路径 + 影响范围 (行数 / 可回滚)
//   ④ Diff    — 折叠面板 (后端尚未提供 diff 数据, 占位提示)
//   ⑤ Actions — 快捷键 footer (Y/N/A) + 决策按钮 (拒绝 / 允许)
//
// Keyboard shortcuts: Y = 允许   N = 拒绝   A = 以后自动允许
// (「以后自动允许」走快捷键, 不再占按钮位 — 视觉更克制,
//  按 qiaomu-design Carbon 七问 "何时用/为什么不用邻近组件")
//
// Lifecycle mirrors ApprovalRequestView (pending / resolved / remembered),
// so the surrounding ProcessTimeline step-fold and the
// `.tl-approval--leaving` collapse animation behave identically.
import { useEffect, useState } from "react";
import {
  Check,
  ChevronDown,
  ChevronRight,
  Clock,
  Layers,
  X,
} from "lucide-react";
import type { ApprovalBatchMessageMeta, ApprovalRequestItem } from "../types";
import { toolVerb } from "./bubbleUtils";

export interface ApprovalBatchViewProps {
  meta: ApprovalBatchMessageMeta;
  onApprove?: (batchId: string, remember?: boolean) => void;
  onDeny?: (batchId: string) => void;
}

type Risk = "dangerous" | "caution" | "safe";

const RISK_ORDER: Record<Risk, number> = { safe: 0, caution: 1, dangerous: 2 };

/** Normalize the server's `RequestApprovalItem::risk` (derived from the
 *  tool registry's `RiskLevel`). Unknown or missing values degrade to
 *  `dangerous` — an unclassifiable call must never be labelled safe. */
function normalizeRisk(raw?: string): Risk {
  const k = (raw ?? "").toLowerCase();
  if (k === "dangerous" || k === "caution" || k === "safe") return k;
  return "dangerous";
}

/** The batch card renders ONE risk chip, so take the most severe item —
 *  a single dangerous call must not be masked by safer siblings. */
function worstRisk(items: ApprovalRequestItem[]): Risk {
  if (items.length === 0) return "dangerous";
  return items.reduce<Risk>((worst, it) => {
    const r = normalizeRisk(it.risk);
    return RISK_ORDER[r] > RISK_ORDER[worst] ? r : worst;
  }, "safe");
}

const RISK_LABEL: Record<Risk, string> = {
  dangerous: "高风险",
  caution: "中风险",
  safe: "低风险",
};

/** Best-effort file path extraction. Many tools store the file path
 *  under one of: `path` / `file` / `filePath` / `target`. */
function extractPath(args: unknown): string | null {
  if (!args || typeof args !== "object" || Array.isArray(args)) return null;
  const a = args as Record<string, unknown>;
  for (const k of ["path", "file", "filePath", "target"]) {
    const v = a[k];
    if (typeof v === "string" && v.trim()) return v.trim();
  }
  return null;
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

  // Tool composition: surface the dominant tool for the header pill.
  const toolCounts = meta.items.reduce<Record<string, number>>((acc, it) => {
    acc[it.tool] = (acc[it.tool] ?? 0) + 1;
    return acc;
  }, {});
  const dominantTool =
    Object.entries(toolCounts).sort((a, b) => b[1] - a[1])[0]?.[0] ?? "tool";
  // Server-authoritative risk (per item) — no tool-name guessing.
  const dominantRisk = worstRisk(meta.items);
  const riskIconCls =
    dominantRisk === "dangerous" ? "is-danger"
    : dominantRisk === "caution" ? "is-warn"
    : "is-safe";

  // File path from dominant item (or first item).
  const primaryItem =
    meta.items.find((it) => it.tool === dominantTool) ?? meta.items[0];
  const filePath = primaryItem ? extractPath(primaryItem.args) : null;
  const fileName = filePath ? filePath.split(/[\\/]/).pop() : null;

  // Diff panel collapsed by default — opens on click. Backend doesn't
  // ship diff data for batch approvals yet, so we surface a placeholder
  // explaining the wire-level gap.
  const [diffOpen, setDiffOpen] = useState(false);

  // Keyboard shortcuts (Y / N / A). Skip when typing in inputs to
  // avoid hijacking text-editing keys. Skipped when decided.
  useEffect(() => {
    if (!isPending) return;
    const handler = (e: KeyboardEvent) => {
      const tag = (e.target as HTMLElement | null)?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA") return;
      if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
      const k = e.key.toLowerCase();
      if (k === "y") {
        e.preventDefault();
        onApprove?.(meta.batchId, false);
      } else if (k === "n") {
        e.preventDefault();
        onDeny?.(meta.batchId);
      } else if (k === "a") {
        e.preventDefault();
        onApprove?.(meta.batchId, true);
      }
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [isPending, meta.batchId, onApprove, onDeny]);

  return (
    <div className={`approval-card footer-block h-enter${decided ? " is-resolved" : ""}`}>
      {/* Pending body — modern 5-layer layout */}
      <div className="approval-full">
        <div className="apv-shell">
          {/* ① Header — 工具图标 + "工具调用" label · 风险 chip · 项数 */}
          <div className="apv-header">
            <div className="apv-header-title">
              <span className={`apv-tool-icon ${riskIconCls}`}>
                <Layers className="icon-md" />
              </span>
              <span className="apv-header-label">工具调用</span>
            </div>
            <div className="apv-header-meta">
              <span className={`risk-chip is-${dominantRisk}`}>
                {RISK_LABEL[dominantRisk]}
              </span>
              <span className="apv-header-sep">·</span>
              <span className="apv-header-count">{count} 项</span>
            </div>
          </div>

          {/* ② Verb row — 动词 + 目标（文件名） */}
          <div className="apv-verb-row">
            <span className="apv-verb">
              {toolVerb(dominantTool, JSON.stringify(primaryItem?.args ?? {}))}
            </span>
            {fileName && <span className="apv-target">{fileName}</span>}
          </div>

          {/* ③ File block — 完整路径 + 影响范围 */}
          {filePath && (
            <div className="apv-file-block">
              <span className="apv-file-path" title={filePath}>
                {filePath}
              </span>
              <span className="apv-file-stats">当前目录 · 可回滚</span>
            </div>
          )}

          {/* ④ Diff toggle + collapsible panel */}
          <button
            type="button"
            className="apv-diff-toggle"
            onClick={() => setDiffOpen((o) => !o)}
            aria-expanded={diffOpen}
            aria-label={diffOpen ? "收起 diff 详情" : "展开 diff 详情"}
          >
            {diffOpen
              ? <ChevronDown className="icon-sm" />
              : <ChevronRight className="icon-sm" />}
            {diffOpen ? "收起 diff" : "查看 diff"}
          </button>
          {diffOpen && (
            <div className="apv-diff-content">
              {(() => {
                // 聚合每项的 diff：file-mutating 项可能有，非文件工具不传。
                const diffBlocks = meta.items
                  .map((it) => {
                    if (!it.diff) return null;
                    const p = extractPath(it.args);
                    const label = p
                      ? p.split(/[\\/]/).pop() ?? it.tool
                      : it.tool;
                    return { tool: it.tool, label, diff: it.diff };
                  })
                  .filter((b): b is { tool: string; label: string; diff: string } => b !== null);
                if (diffBlocks.length === 0) {
                  return (
                    <span className="apv-diff-placeholder">
                      本批次无可显示的 diff（非文件操作 / 后端未生成）
                    </span>
                  );
                }
                return diffBlocks.map((b, idx) => (
                  <div key={idx} className="apv-diff-block">
                    <div className="apv-diff-header">
                      <span className="apv-diff-file">{b.label}</span>
                      <span className="apv-diff-kind">{b.tool}</span>
                    </div>
                    <pre className="apv-cmd-pre apv-diff-pre">{b.diff}</pre>
                  </div>
                ));
              })()}
            </div>
          )}

          {/* ⑤ Actions — 快捷键 footer + 决策按钮 */}
          {isPending && (
            <div className="apv-actions">
              <div className="apv-shortcut" aria-hidden="true">
                <span><kbd>Y</kbd>允许</span>
                <span><kbd>N</kbd>拒绝</span>
                <span><kbd>A</kbd>以后自动</span>
              </div>
              <div className="apv-btns">
                <button
                  type="button"
                  className="apv-btn apv-btn-danger"
                  aria-label="拒绝本次工具调用（快捷键 N）"
                  onClick={() => onDeny?.(meta.batchId)}
                >
                  拒绝
                </button>
                <button
                  type="button"
                  className="apv-btn apv-btn-primary"
                  aria-label="允许本次工具调用（快捷键 Y）"
                  onClick={() => onApprove?.(meta.batchId)}
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
          <span
            className={`approval-summary-icon approval-summary-icon-${
              approved ? "ok" : isExpired ? "expired" : "err"
            }`}
          >
            {approved
              ? <Check className="icon-sm" />
              : isExpired
                ? <Clock className="icon-sm" />
                : <X className="icon-sm" />}
          </span>
          <span className="approval-summary-tool">{count} 个调用</span>
          <span className="approval-summary-sep">·</span>
          <span className="approval-summary-desc">批量决策</span>
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
// `describeToolArgs` / `formatArgsBrief` retained in import for
// future per-item fallback rendering when batch spans heterogeneous
// tool families.
