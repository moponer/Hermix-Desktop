import { Bot, Sparkles, Loader2, CheckCircle2, AlertCircle } from "lucide-react";
import type { SubAgentState, SubAgentPhase } from "../types";

/**
 * Real-time "子代理" status panel. Renders one row per delegated
 * sub-agent (`delegate_task`) keyed by `subAgentId`, showing its task
 * goal, lifecycle phase badge (已启动 / 执行中 / 已完成 / 失败), role,
 * turn count and estimated cost. Driven by `SubAgentEvent` WS events
 * (see `useStreamChat` → `onSubAgentEvent`).
 *
 * Uses the project's established info/success/error status tokens
 * (no blue/purple per design rules) and gray/zinc surfaces.
 *
 * Light-theme fix: removed `opacity` inline styles that
 * killed contrast on white backgrounds (0.55 opacity on #fafafa =
 * nearly invisible). Now uses explicit CSS variables for muted text
 * (--color-text-muted / --color-text-faint) instead of opacity tricks.
 * Also fixed dark fallback colors in CSS var defaults — these were
 * hard-coded dark values (#1c1c20, #e5e7eb) that would render
 * light-on-light / dark-on-dark if the CSS var failed to resolve.
 */

const PHASE_CONFIG: Record<
  SubAgentPhase,
  { label: string; bg: string; text: string; icon: typeof Loader2 }
> = {
  spawned: { label: "已启动", bg: "var(--color-accent-soft-strong)", text: "var(--color-accent)", icon: Bot },
  started: { label: "执行中", bg: "var(--color-accent-soft-strong)", text: "var(--color-accent)", icon: Loader2 },
  done: { label: "已完成", bg: "var(--color-success-soft)", text: "var(--color-success)", icon: CheckCircle2 },
  failed: { label: "失败", bg: "var(--color-error-soft)", text: "var(--color-error)", icon: AlertCircle },
};

const ROLE_LABEL: Record<string, string> = {
  general: "通用",
  explore: "探索",
  plan: "规划",
};

export function SubAgentPanel({ subAgents }: { subAgents: Record<string, SubAgentState> }) {
  const list = Object.values(subAgents);
  if (list.length === 0) return null;

  return (
    <div
      className="sub-agent-panel footer-block"
      style={{
        background: "var(--color-bg-secondary)",
        border: "1px solid var(--color-border-secondary)",
        borderRadius: 12,
        padding: "10px 12px",
        // Vertical margin only — horizontal centering + width are
        // handled by `.footer-block` so all three trailing blocks
        // share the same horizontal lane.
        marginTop: 10,
        marginBottom: 10,
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 8 }}>
        <Sparkles className="icon-md" style={{ color: "var(--color-accent)" }} />
        <span style={{ fontWeight: 600, fontSize: 13, color: "var(--color-text-primary)" }}>
          子代理
        </span>
        <span style={{ fontSize: 11, color: "var(--color-text-faint)" }}>
          {list.length}
        </span>
      </div>
      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        {list.map((sa) => {
          const cfg = PHASE_CONFIG[sa.phase];
          const Icon = cfg.icon;
          return (
            <div
              key={sa.subAgentId}
              style={{
                background: "var(--color-bg-tertiary)",
                borderRadius: 8,
                padding: "8px 10px",
              }}
            >
              <div
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "space-between",
                  gap: 8,
                }}
              >
                <span
                  title={sa.task}
                  style={{
                    fontSize: 12.5,
                    color: "var(--color-text-primary)",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                    flex: 1,
                    minWidth: 0,
                  }}
                >
                  {sa.task || "（无任务描述）"}
                </span>
                <span
                  className="inline-flex items-center gap-1 rounded-full font-semibold"
                  style={{
                    background: cfg.bg,
                    color: cfg.text,
                    padding: "2px 8px",
                    fontSize: 10.5,
                    flexShrink: 0,
                  }}
                >
                  <Icon className={`icon-sm ${sa.phase === "started" ? "animate-spin shrink-0" : undefined}`} />
                  {cfg.label}
                </span>
              </div>
              <div
                style={{
                  display: "flex",
                  alignItems: "center",
                  gap: 10,
                  marginTop: 5,
                  fontSize: 11,
                  color: "var(--color-text-muted)",
                }}
              >
                {sa.role && <span>{ROLE_LABEL[sa.role] ?? sa.role}</span>}
                {typeof sa.turns === "number" && <span>轮次 {sa.turns}</span>}
                {typeof sa.costUsd === "number" && <span>≈ ¥{sa.costUsd.toFixed(4)}</span>}
              </div>
              {/* Rolling step log — one line per internal action (tool
                  call / thinking), newest at the bottom. The backend
                  sends the last N entries (SUB_AGENT_STEP_LOG_CAP = 6);
                  older rows fall off, giving a terminal-style scroll. */}
              {Array.isArray(sa.recentSteps) && sa.recentSteps.length > 0 && (
                <div
                  style={{
                    marginTop: 6,
                    borderTop: "1px solid var(--color-border-secondary)",
                    paddingTop: 5,
                    display: "flex",
                    flexDirection: "column",
                    gap: 2,
                  }}
                >
                  {sa.recentSteps.map((s, i) => (
                    <div
                      key={`${sa.subAgentId}-step-${i}`}
                      style={{
                        fontSize: 10.5,
                        fontFamily: 'ui-monospace, "Cascadia Code", Menlo, monospace',
                        color:
                          i === sa.recentSteps!.length - 1 && sa.phase === "started"
                            ? "var(--color-accent)"
                            : "var(--color-text-faint)",
                        whiteSpace: "nowrap",
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        lineHeight: 1.5,
                      }}
                      title={s}
                    >
                      {s}
                    </div>
                  ))}
                </div>
              )}
              {sa.phase === "failed" && sa.error && (
                <div
                  style={{
                    marginTop: 5,
                    fontSize: 11,
                    color: "var(--color-error)",
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                  }}
                  title={sa.error}
                >
                  {sa.error}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
