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
 */

const PHASE_CONFIG: Record<
  SubAgentPhase,
  { label: string; bg: string; text: string; icon: typeof Loader2 }
> = {
  spawned: { label: "已启动", bg: "var(--color-info-soft)", text: "var(--color-info)", icon: Bot },
  started: { label: "执行中", bg: "var(--color-info-soft)", text: "var(--color-info)", icon: Loader2 },
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
      className="sub-agent-panel"
      style={{
        background: "var(--color-bg-secondary, #1c1c20)",
        border: "1px solid var(--color-bg-tertiary, #2e2e34)",
        borderRadius: 12,
        padding: "10px 12px",
        margin: "10px 0",
      }}
    >
      <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 8 }}>
        <Sparkles size={14} style={{ color: "var(--color-info, #94a3b8)" }} />
        <span style={{ fontWeight: 600, fontSize: 13, color: "var(--color-text, #e5e7eb)" }}>
          子代理
        </span>
        <span style={{ fontSize: 11, color: "var(--color-text, #e5e7eb)", opacity: 0.5 }}>
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
                background: "var(--color-bg-tertiary, #2e2e34)",
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
                    color: "var(--color-text, #e5e7eb)",
                    opacity: 0.92,
                    overflow: "hidden",
                    textOverflow: "ellipsis",
                    whiteSpace: "nowrap",
                    flex: 1,
                    minWidth: 0,
                  }}
                >
                  {sa.task || "(无任务描述)"}
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
                  <Icon size={11} />
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
                  color: "var(--color-text, #e5e7eb)",
                  opacity: 0.55,
                }}
              >
                {sa.role && <span>{ROLE_LABEL[sa.role] ?? sa.role}</span>}
                {typeof sa.turns === "number" && <span>轮次 {sa.turns}</span>}
                {typeof sa.costUsd === "number" && <span>≈ ¥{sa.costUsd.toFixed(4)}</span>}
              </div>
              {sa.phase === "failed" && sa.error && (
                <div
                  style={{
                    marginTop: 5,
                    fontSize: 11,
                    color: "var(--color-error, #ef4444)",
                    opacity: 0.85,
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
