/**
 * AwakeningCard — top-of-stack summary for the awakening phase.
 *
 * Driven by TickLoop: when awakening starts, the agent emits
 *   { op: "show",   id, type: "awakening",
 *     props: { phase, subphase?, tick, totalTicks, healthy, startedAt, completed? } }
 *
 * After each tick, the agent emits an update patch with the latest
 * `tick` / `subphase` / `healthy` so the card animates in place.
 *
 * When `completed: true` lands, the card flips to its "done" footer
 * with totals + total elapsed time.
 *
 * Visual model:
 *   - Header: phase icon + name + tick counter + live spinner.
 *   - Progress dots (filled = done, dim = pending).
 *   - Sub-phase strip: current sub-phase label + step counter.
 *   - Footer: live healthy/warning status + completed totals.
 */

import { useEffect, useState } from "react";
import { Sparkles, CheckCircle2, AlertTriangle, Loader2 } from "lucide-react";
import type { AcuiComponent } from "../AcuiRegistry";

type Props = {
  phase?: string;
  subphase?: string;
  tick?: number;
  totalTicks?: number;
  healthy?: boolean;
  /** ISO timestamp of awakening start; used to compute elapsed ms. */
  startedAt?: string;
  /** When true, the agent has finished its awakening sequence. */
  completed?: boolean;
  /** Totals summary — populated on `completed: true`. */
  totals?: { ok?: number; warn?: number; fail?: number; skip?: number };
};

export const AwakeningCard: AcuiComponent = ({ show }) => {
  const p = ((show.props ?? {}) as Props) ?? {};

  const tick = p.tick ?? 0;
  const totalTicks = p.totalTicks ?? 10;
  const healthy = p.healthy ?? true;
  const phase = p.phase ?? "觉醒中";
  const subphase = p.subphase ?? null;
  const completed = p.completed ?? false;
  const totals = p.totals ?? null;

  // Pulse on each tick update so the user sees live progress.
  const [pulse, setPulse] = useState(0);
  useEffect(() => {
    setPulse((n) => n + 1);
  }, [tick]);

  // Tick the elapsed display while running so the user sees time
  // advancing. We render the elapsed value rounded down to the
  // nearest second for display, but we only re-render every 5s —
  // going to 1Hz made this card re-render 5x more often than the
  // user could perceive, and on the main chat surface that adds up
  // to a noticeable amount of background React work.
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (completed) return;
    const id = setInterval(() => setNow(Date.now()), 5000);
    return () => clearInterval(id);
  }, [completed]);

  const startedAtMs = p.startedAt ? new Date(p.startedAt).getTime() : null;
  const elapsedMs =
    startedAtMs != null && !Number.isNaN(startedAtMs)
      ? Math.max(0, now - startedAtMs)
      : null;

  return (
    <div
      data-acui-kind="awakening"
      data-acui-completed={completed || undefined}
      className={`relative px-3.5 py-2.5 rounded-xl border h-enter transition-colors ${
        completed
          ? healthy
            ? "bg-emerald-500/5 border-emerald-500/20"
            : "bg-amber-500/5 border-amber-500/20"
          : "bg-accent/[0.06] border-accent/20"
      }`}
    >
      {/* Header: phase name + tick counter + live spinner */}
      <div className="flex items-center gap-2 mb-1.5">
        {completed ? (
          healthy ? (
            <CheckCircle2 className="icon-sm text-emerald-300 shrink-0" />
          ) : (
            <AlertTriangle className="icon-sm text-amber-300 shrink-0" />
          )
        ) : (
          <Sparkles className="icon-sm text-accent shrink-0" />
        )}
        <span className="text-[length:var(--text-ui)] font-semibold text-text-primary truncate">
          {phase}
        </span>
        {subphase && !completed && (
          <span className="text-[length:var(--text-meta)] text-text-muted truncate max-w-[160px]" title={subphase}>
            · {subphase}
          </span>
        )}
        <span className="ml-auto text-[length:var(--text-meta)] text-text-muted font-mono shrink-0">
          {tick}/{totalTicks}
        </span>
        {!completed && (
          <Loader2 className="icon-sm text-accent animate-spin shrink-0" />
        )}
      </div>

      {/* Progress dots — capped at 30, overflow shown as "+N" */}
      <div
        className="flex items-center gap-1 mb-1.5 flex-wrap"
        data-pulse={pulse}
      >
        {Array.from({ length: Math.min(totalTicks, 30) }).map((_, i) => {
          const filled = i < tick;
          const isCurrent = i === tick - 1 && !completed;
          return (
            <div
              key={`tick-${i}`}
              className={`h-1 rounded-full transition-colors duration-slow ${
                isCurrent
                  ? "w-3 bg-accent"
                  : filled
                    ? "flex-1 min-w-[6px] bg-accent/70"
                    : "flex-1 min-w-[6px] bg-white/[0.06]"
              }`}
            />
          );
        })}
        {totalTicks > 30 && (
          <div className="text-[length:var(--text-meta)] text-text-faint ml-1">
            +{totalTicks - 30}
          </div>
        )}
      </div>

      {/* Footer: status + (running: elapsed-so-far) | (completed: totals) */}
      <div className="flex items-center gap-1.5 text-[length:var(--text-meta)]">
        {completed ? (
          totals ? (
            <span className="font-mono text-text-secondary">
              {totals.ok ?? 0} 通过
              {(totals.warn ?? 0) > 0 && ` · ${totals.warn} 警告`}
              {(totals.fail ?? 0) > 0 && ` · ${totals.fail} 失败`}
              {(totals.skip ?? 0) > 0 && ` · ${totals.skip} 跳过`}
            </span>
          ) : healthy ? (
            <span className="text-text-muted">系统就绪</span>
          ) : (
            <span className="text-amber-200/80">部分检查异常</span>
          )
        ) : healthy ? (
          <>
            <Loader2 className="icon-xs text-zinc-300 animate-spin shrink-0" />
            <span className="text-text-muted">自检中…</span>
          </>
        ) : (
          <>
            <AlertTriangle className="icon-xs text-amber-300 shrink-0" />
            <span className="text-amber-200/80">部分检查异常</span>
          </>
        )}
        {elapsedMs != null && (
          <>
            <span className="text-text-muted">·</span>
            <span className="font-mono text-text-faint">{formatElapsed(elapsedMs)}</span>
          </>
        )}
      </div>
    </div>
  );
};

function formatElapsed(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const min = Math.floor(ms / 60_000);
  const sec = Math.floor((ms % 60_000) / 1000);
  return `${min}m${sec}s`;
}