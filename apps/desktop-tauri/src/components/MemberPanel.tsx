import { CheckCircle2, CircleAlert, CircleX, KanbanSquare, Loader2, ShieldCheck } from "lucide-react";
import type { MemberTicketState } from "../types";

/**
 * 实时「成员工单」状态面板（总管方案 P4.4）。与 `SubAgentPanel`（子代理 =
 * 数据通道）平行但独立：成员是独立会话的执行单元，事件来自
 * `MemberEvent` WS 流（`member_update` extra → `ws.rs` 桥接），按
 * `memberId` 累积、按相位列徽标。视觉与 SubAgentPanel 同源（令牌类），
 * 避免同屏两套面板语言。
 */
const PHASE_CONFIG: Record<string, { label: string; badge: string; icon: typeof Loader2 }> = {
  dispatched: { label: "已派出", badge: "bg-accent-soft text-accent", icon: KanbanSquare },
  running: { label: "执行中", badge: "bg-accent-soft text-accent", icon: Loader2 },
  done: { label: "已完成", badge: "bg-success-soft text-text-success", icon: CheckCircle2 },
  review_pass: { label: "验收通过", badge: "bg-success-soft text-text-success", icon: ShieldCheck },
  review_warn: { label: "有保留通过", badge: "bg-warning-soft text-text-warning", icon: ShieldCheck },
  review_reject: { label: "验收未过", badge: "bg-error-soft text-text-error", icon: CircleX },
  review_timeout: { label: "验收超时", badge: "bg-warning-soft text-text-warning", icon: CircleAlert },
  failed: { label: "失败", badge: "bg-error-soft text-text-error", icon: CircleAlert },
  timeout: { label: "超时", badge: "bg-warning-soft text-text-warning", icon: CircleAlert },
  cancelled: { label: "已中止", badge: "bg-bg-tertiary text-text-muted", icon: CircleX },
  stopped: { label: "已停止", badge: "bg-bg-tertiary text-text-muted", icon: CircleX },
};

function phaseConfig(phase: string) {
  return PHASE_CONFIG[phase] ?? { label: phase, badge: "bg-bg-tertiary text-text-muted", icon: KanbanSquare };
}

export function MemberPanel({ tickets }: { tickets: Record<string, MemberTicketState> }) {
  const list = Object.values(tickets);
  if (list.length === 0) return null;

  return (
    <div className="sub-agent-panel footer-block mb-2.5 mt-2.5 rounded-[var(--radius-lg)] bg-[var(--color-bg-secondary)] px-3 py-2.5">
      <div className="mb-2 flex items-center gap-1.5">
        <KanbanSquare className="icon-md text-accent" />
        <span className="text-[length:var(--text-ui)] font-semibold text-[var(--color-text-primary)]">
          成员工单
        </span>
        <span className="text-[length:var(--text-tiny)] text-[var(--color-text-faint)]">{list.length}</span>
      </div>
      <div className="flex flex-col gap-2">
        {list.map((t) => {
          const cfg = phaseConfig(t.phase);
          const Icon = cfg.icon;
          return (
            <div key={t.memberId} className="rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] px-2.5 py-2">
              <div className="flex items-center justify-between gap-2">
                <span
                  title={t.task}
                  className="min-w-0 flex-1 truncate text-[length:var(--text-meta)] text-[var(--color-text-primary)]"
                >
                  {t.task || "（无任务描述）"}
                </span>
                <span
                  className={`inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[length:var(--text-tiny)] font-semibold ${cfg.badge}`}
                >
                  <Icon className={`icon-sm ${t.phase === "running" ? "shrink-0 animate-spin" : ""}`} />
                  {cfg.label}
                </span>
              </div>
              <div className="mt-1 flex items-center gap-2.5 text-[length:var(--text-tiny)] text-[var(--color-text-muted)]">
                <span className="font-mono">{t.memberId.slice(0, 13)}</span>
                {typeof t.turns === "number" && <span>轮次 {t.turns}</span>}
              </div>
              {Array.isArray(t.recentSteps) && t.recentSteps.length > 0 && (
                <div className="mt-1.5 flex flex-col gap-0.5 pt-1">
                  {t.recentSteps.map((s, i) => (
                    <div
                      key={`${t.memberId}-step-${i}`}
                      title={s}
                      className={`truncate font-mono text-[length:var(--text-tiny)] leading-snug ${
                        i === t.recentSteps!.length - 1 && t.phase === "running"
                          ? "text-accent"
                          : "text-[var(--color-text-faint)]"
                      }`}
                    >
                      {s}
                    </div>
                  ))}
                </div>
              )}
              {t.error && (
                <div
                  title={t.error}
                  className="mt-1 truncate text-[length:var(--text-tiny)] text-[var(--color-text-error)]"
                >
                  {t.error}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
