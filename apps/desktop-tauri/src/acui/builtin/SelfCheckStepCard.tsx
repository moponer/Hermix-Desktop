/**
 * SelfCheckStepCard — renders one self-check step (tick) with
 * optional detail expansion.
 *
 * Driven by TickLoop's awakening phase. The agent emits
 *   { op: "show",   id, type: "self_check_step", props: { step, total, label, status, metrics?, error?, durationMs? } }
 *   { op: "update", id, patch: { status, metrics, error, durationMs } }
 *
 * Status values: "running" | "ok" | "warn" | "fail" | "skip"
 *
 * Visual model:
 *   - A numbered circle with a status glyph, label, and step counter.
 *   - When the user clicks the expand chevron, an additional row
 *     appears with:
 *       * `metrics` (key/value pairs, monospace, dim)
 *       * `error` (red, when status === "fail")
 *       * `durationMs` (right-aligned, monospace, when terminal)
 *   - Steps render in a vertical stack with a thin connector line
 *     between adjacent entries (rendered by the parent panel's
 *     CSS — `data-acui-section="detail"` items get the connector
 *     via `> * + *` selector on the wrapper).
 */

import { useState } from "react";
import { Check, AlertTriangle, X, Loader2, Minus, ChevronDown, ChevronRight } from "lucide-react";
import type { AcuiComponent } from "../AcuiRegistry";

export type SelfCheckStatus = "running" | "ok" | "warn" | "fail" | "skip";

const STATUS_GLYPH: Record<SelfCheckStatus, { Icon: typeof Check; cls: string; label: string }> = {
  running: { Icon: Loader2, cls: "text-zinc-300 animate-spin", label: "进行中" },
  ok:      { Icon: Check,   cls: "text-emerald-300",            label: "通过" },
  warn:    { Icon: AlertTriangle, cls: "text-amber-300",         label: "警告" },
  fail:    { Icon: X,       cls: "text-rose-300",               label: "失败" },
  skip:    { Icon: Minus,   cls: "text-text-muted",             label: "跳过" },
};

type Props = {
  step?: number;
  total?: number;
  label?: string;
  status?: SelfCheckStatus;
  /** Optional metrics (any key/value pairs) shown when expanded. */
  metrics?: Record<string, string | number | boolean>;
  /** Optional error message (rendered red when status === "fail"). */
  error?: string;
  /** Optional duration in milliseconds. */
  durationMs?: number;
};

export const SelfCheckStepCard: AcuiComponent = ({ show }) => {
  const p = ((show.props ?? {}) as Props) ?? {};
  const step = p.step ?? 0;
  const total = p.total ?? 10;
  const label = p.label ?? "";
  const status: SelfCheckStatus = p.status ?? "running";
  const { Icon, cls, label: statusLabel } = STATUS_GLYPH[status];

  const isTerminal = status !== "running";
  const hasDetails =
    (p.metrics && Object.keys(p.metrics).length > 0) ||
    (typeof p.error === "string" && p.error.length > 0) ||
    typeof p.durationMs === "number";

  const [expanded, setExpanded] = useState(false);
  const toggle = () => {
    if (hasDetails) setExpanded((v) => !v);
  };

  return (
    <div
      data-acui-kind="self_check_step"
      data-acui-status={status}
      data-acui-step={step}
      className={`relative flex flex-col gap-1 px-3 py-1.5 rounded-md border h-enter transition-colors ${
        status === "fail"
          ? "bg-rose-500/[0.04] border-rose-500/15"
          : status === "warn"
            ? "bg-amber-500/[0.04] border-amber-500/15"
            : "bg-white/[0.02] border-white/[0.05]"
      }`}
    >
      {/* Connector line — a vertical line on the left side that visually
          links adjacent steps. Only render on rows that aren't the very
          first; the parent panel adds a `data-acui-section="detail"`
          wrapper that we hook into via :first-child. */}
      <div
        aria-hidden
        className="absolute left-[18px] top-0 bottom-0 w-px bg-white/[0.04] data-[first=true]:hidden"
        data-first={step <= 1 ? "true" : "false"}
      />

      {/* Header row */}
      <button
        type="button"
        onClick={toggle}
        disabled={!hasDetails}
        className={`flex items-center gap-2.5 w-full text-left ${
          hasDetails ? "cursor-pointer" : "cursor-default"
        }`}
        aria-expanded={expanded}
      >
        <div
          className={`shrink-0 w-6 h-6 rounded-full flex items-center justify-center bg-black/30 border font-mono text-[10px] ${
            status === "ok"
              ? "border-emerald-500/30 text-emerald-200"
              : status === "fail"
                ? "border-rose-500/40 text-rose-200"
                : status === "warn"
                  ? "border-amber-500/30 text-amber-200"
                  : status === "skip"
                    ? "border-white/10 text-text-faint"
                    : "border-white/[0.08] text-text-secondary"
          }`}
        >
          {step}
        </div>
        <div className="flex-1 min-w-0 truncate text-[12px] text-text-primary">
          {label || `第 ${step} 步自检`}
        </div>
        <div className={`shrink-0 flex items-center gap-1 text-[11px] ${cls}`}>
          <Icon size={11} />
          <span>{statusLabel}</span>
        </div>
        {isTerminal && typeof p.durationMs === "number" && (
          <div className="shrink-0 text-[10px] text-text-faint font-mono">
            {formatDuration(p.durationMs)}
          </div>
        )}
        <div className="shrink-0 text-[10px] text-text-faint font-mono">
          {step}/{total}
        </div>
        {hasDetails ? (
          expanded ? (
            <ChevronDown size={11} className="shrink-0 text-text-faint" />
          ) : (
            <ChevronRight size={11} className="shrink-0 text-text-faint" />
          )
        ) : (
          <div className="w-[11px] shrink-0" />
        )}
      </button>

      {/* Detail row — only when expanded. */}
      {expanded && hasDetails && (
        <div
          data-acui-detail="true"
          className="ml-8 pl-3 border-l border-white/[0.04] space-y-1"
        >
          {p.metrics && Object.keys(p.metrics).length > 0 && (
            <div className="grid grid-cols-[max-content_1fr] gap-x-3 gap-y-0.5 text-[10.5px] font-mono">
              {Object.entries(p.metrics).map(([k, v]) => (
                <div key={k} className="contents">
                  <div className="text-text-faint">{k}</div>
                  <div className="text-text-secondary truncate">{String(v)}</div>
                </div>
              ))}
            </div>
          )}
          {status === "fail" && typeof p.error === "string" && p.error.length > 0 && (
            <div className="text-[10.5px] text-rose-200/90 font-mono whitespace-pre-wrap break-all">
              {p.error}
            </div>
          )}
          {!p.metrics &&
            !(status === "fail" && p.error) &&
            typeof p.durationMs === "number" && (
              <div className="text-[10.5px] text-text-faint font-mono">
                耗时 {formatDuration(p.durationMs)}
              </div>
            )}
        </div>
      )}
    </div>
  );
};

function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)}s`;
  const min = Math.floor(ms / 60_000);
  const sec = Math.floor((ms % 60_000) / 1000);
  return `${min}m${sec}s`;
}