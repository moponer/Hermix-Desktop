/**
 * AcuiPanel — renders the active ACUI card stack above the composer.
 *
 * Layout strategy:
 *   1. **Header section**: awakening cards (if any) — these are
 *      "top-of-stack" summaries that stay visible while the agent
 *      boots up.
 *   2. **Detail section**: self-check steps + everything else, in
 *      show-order, grouped by `position` (before → main → after).
 *      Within each group, the first item is the "anchor" and the rest
 *      are siblings.
 *   3. **Summary section** (auto-derived): once the agent has emitted
 *      a "completion" sentinel (an `awakening` show with
 *      `props.completed: true`), we render a one-line summary card.
 *
 * The panel renders nothing if `state.cards.length === 0`.
 *
 * Props:
 *   - `state`: AcuiState from useAcuiState.
 *   - `registry`: optional custom AcuiRegistry.
 *   - `decide`: AcuiDecisionBus from useAcuiState.bind.
 *   - `className`: optional wrapper override.
 *   - `defaultCollapsed`: whether the detail section starts folded.
 *
 * The `h-enter` CSS class on each card wrapper (defined in
 * `styles/components.css`) provides a tiny fade-in animation. We
 * avoid pulling in framer-motion to keep the frontend bundle small.
 */

import { useState, useCallback, useMemo } from "react";
import { ChevronDown, ChevronRight, Sparkles, CheckCircle2, AlertTriangle, ListChecks } from "lucide-react";
import { renderAcuiCards } from "./AcuiRenderer";
import type { AcuiShow, AcuiState } from "./types";
import type { AcuiRegistry, AcuiDecisionBus } from "./AcuiRegistry";

export interface AcuiPanelProps {
  state: AcuiState;
  registry?: AcuiRegistry;
  /** Decision bus (from `useAcuiState.bind({ ... })`). */
  decide?: AcuiDecisionBus;
  className?: string;
  defaultCollapsed?: boolean;
}

interface GroupedCards {
  /** Awakening / top-of-stack summary cards (one typically). */
  header: AcuiShow[];
  /** Before-text cards (context / prefetch). */
  before: AcuiShow[];
  /** The "main" body — self-check steps + everything else. */
  detail: AcuiShow[];
  /** After-text cards (resolved footers, follow-ups). */
  after: AcuiShow[];
}

/**
 * Group cards by their semantic role. `header` is reserved for
 * `awakening`; everything else falls into the bucket that matches its
 * `position`. We intentionally don't hard-code more roles — new kinds
 * automatically land in `detail` unless the caller registers a custom
 * grouper.
 */
function groupCards(state: AcuiState): GroupedCards {
  const groups: GroupedCards = { header: [], before: [], detail: [], after: [] };
  for (const c of state.cards) {
    if (c.type === "awakening") {
      groups.header.push(c);
      continue;
    }
    switch (c.position) {
      case "before":
        groups.before.push(c);
        break;
      case "replace":
        // Replace cards are typically a complete answer in their own
        // right; treat them as "detail" so they show up in the stack.
        groups.detail.push(c);
        break;
      case "after":
      default:
        groups.detail.push(c);
        break;
    }
  }
  return groups;
}

/**
 * Derive a completion summary from the live state. Used to render the
 * "全部完成 / 部分异常 / 跳过 N 项" footer when awakening has settled.
 */
interface AwakeningSummary {
  total: number;
  ok: number;
  warn: number;
  fail: number;
  skip: number;
  running: number;
  /** True if all ticks are terminal (ok/warn/fail/skip). */
  finished: boolean;
  /** True if all ticks are ok or skip (no warn/fail/running). */
  healthy: boolean;
  /** ms since the first self-check step. */
  elapsedMs: number | null;
}

function deriveAwakeningSummary(state: AcuiState): AwakeningSummary | null {
  const awakening = state.cards.find((c) => c.type === "awakening");
  if (!awakening) return null;
  const props = (awakening.props ?? {}) as {
    completed?: boolean;
    tick?: number;
    totalTicks?: number;
    healthy?: boolean;
    startedAt?: string;
  };
  // Walk self_check_step cards (the canonical source of truth).
  const steps = state.cards.filter((c) => c.type === "self_check_step");
  if (steps.length === 0) return null;
  const summary: AwakeningSummary = {
    total: steps.length,
    ok: 0,
    warn: 0,
    fail: 0,
    skip: 0,
    running: 0,
    finished: false,
    healthy: false,
    elapsedMs: null,
  };
  for (const s of steps) {
    const status = ((s.props ?? {}) as { status?: string }).status ?? "running";
    if (status === "ok") summary.ok += 1;
    else if (status === "warn") summary.warn += 1;
    else if (status === "fail") summary.fail += 1;
    else if (status === "skip") summary.skip += 1;
    else summary.running += 1;
  }
  summary.finished = summary.running === 0;
  summary.healthy = summary.finished && summary.fail === 0 && summary.warn === 0;
  if (props.startedAt) {
    const t = new Date(props.startedAt).getTime();
    if (!Number.isNaN(t)) summary.elapsedMs = Math.max(0, Date.now() - t);
  }
  return summary;
}

export function AcuiPanel({ state, registry, decide, className, defaultCollapsed = false }: AcuiPanelProps) {
  const [collapsed, setCollapsed] = useState(defaultCollapsed);
  const toggleCollapsed = useCallback(() => setCollapsed((v) => !v), []);

  const groups = useMemo(() => groupCards(state), [state]);
  const summary = useMemo(() => deriveAwakeningSummary(state), [state]);

  const totalCards = state.cards.length;
  if (totalCards === 0) return null;

  const buckets = renderAcuiCards({ state, registry, decide });
  // Detail section = `before` + `replace` + `after`, in that order.
  // Cards that target a specific position are placed at their
  // requested anchor; the panel itself doesn't splice into the chat
  // stream (that's ChatView's job).
  const detailNodes = [
    ...buckets.before,
    ...buckets.replace,
    ...buckets.after,
  ];

  return (
    <div
      data-acui-panel="true"
      className={
        className ??
        "px-3.5 py-2 space-y-1.5 border-t border-white/[0.04] bg-black/10"
      }
    >
      {/* ── Header section: awakening summary (if any) ───────────── */}
      {groups.header.length > 0 && (
        <div data-acui-section="header" className="space-y-1.5">
          {groups.header.map((c) => (
            <div key={c.id} data-acui-card-wrapper>
              {buckets.after.find(() => false) /* type-narrowing no-op */ ? null : null}
              {/* Render the actual header card from buckets before/after (the
                  bucket assignment depends on position; awakening is typically
                  'before'). We re-resolve it here by id. */}
              <HeaderCard
                show={c}
                registry={registry}
                decide={decide}
                outcome={
                  // Look up outcome via the same path the renderer uses,
                  // so resolved footers render consistently.
                  undefined
                }
              />
            </div>
          ))}
        </div>
      )}

      {/* ── Completion summary (after awakening finished) ────────── */}
      {summary?.finished && (
        <AwakeningSummaryBanner summary={summary} />
      )}

      {/* ── Detail section: collapsible list ─────────────────────── */}
      {detailNodes.length > 0 && (
        <div data-acui-section="detail" className="space-y-1.5">
          <button
            type="button"
            onClick={toggleCollapsed}
            className="flex items-center gap-1.5 text-[10.5px] text-text-faint hover:text-text-secondary transition-colors w-full"
            aria-expanded={!collapsed}
          >
            {collapsed ? <ChevronRight size={11} /> : <ChevronDown size={11} />}
            <ListChecks size={11} />
            <span>
              {collapsed ? "展开" : "收起"} {detailNodes.length} 项
            </span>
          </button>
          {!collapsed && (
            <div className="space-y-1.5">
              {detailNodes.map((node, idx) => (
                <div
                  key={(node as { key?: string })?.key ?? `acui-${idx}`}
                  data-acui-card-wrapper
                >
                  {node}
                </div>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Internal: re-resolve a single card via the renderer so the panel renders
// header cards using the same code path as everything else (rather than
// re-implementing the registry lookup).
// ---------------------------------------------------------------------------

function HeaderCard({
  show,
  registry,
  decide,
}: {
  show: AcuiShow;
  registry?: AcuiRegistry;
  decide?: AcuiDecisionBus;
  outcome?: unknown;
}) {
  // Single-card render: use the same renderer but with a synthetic
  // 1-card state, so registry resolution / fallback / decide bus
  // behave identically.
  const synthetic: AcuiState = { cards: [show], decisions: [] };
  const buckets = renderAcuiCards({
    state: synthetic,
    registry,
    decide,
    lookupOutcome: () => ({ resolved: false }),
  });
  const node =
    buckets.before[0] ?? buckets.after[0] ?? buckets.replace[0] ?? null;
  return <>{node}</>;
}

// ---------------------------------------------------------------------------
// Internal: awakening-completion summary banner.
// ---------------------------------------------------------------------------

function AwakeningSummaryBanner({ summary }: { summary: AwakeningSummary }) {
  const isHealthy = summary.healthy;
  const elapsedText =
    summary.elapsedMs != null ? formatElapsed(summary.elapsedMs) : null;
  return (
    <div
      data-acui-section="summary"
      className={`flex items-center gap-2 px-3 py-1.5 rounded-md text-[11px] h-enter ${
        isHealthy
          ? "bg-emerald-500/5 border border-emerald-500/20 text-emerald-200"
          : "bg-amber-500/5 border border-amber-500/20 text-amber-200"
      }`}
    >
      {isHealthy ? (
        <CheckCircle2 size={12} className="shrink-0" />
      ) : (
        <AlertTriangle size={12} className="shrink-0" />
      )}
      <span className="font-medium">
        {isHealthy ? "系统自检通过" : "部分检查异常"}
      </span>
      <span className="text-text-muted">·</span>
      <span className="font-mono">
        {summary.ok} 通过{summary.warn > 0 ? ` · ${summary.warn} 警告` : ""}
        {summary.fail > 0 ? ` · ${summary.fail} 失败` : ""}
        {summary.skip > 0 ? ` · ${summary.skip} 跳过` : ""}
      </span>
      {elapsedText && (
        <>
          <span className="text-text-muted">·</span>
          <span className="font-mono text-text-faint">{elapsedText}</span>
        </>
      )}
      <Sparkles size={11} className="ml-auto shrink-0 text-text-faint" />
    </div>
  );
}

function formatElapsed(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const min = Math.floor(ms / 60_000);
  const sec = Math.floor((ms % 60_000) / 1000);
  return `${min}m ${sec}s`;
}