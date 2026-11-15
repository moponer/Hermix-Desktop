import { Bot, Sparkles, Loader2, CheckCircle2, AlertCircle } from "lucide-react";
import type { SubAgentState, SubAgentPhase } from "../types";
import { COMPLETED } from "./strings";

/**
 * Real-time "子代理" status panel. Renders one row per delegated
 * sub-agent (`delegate_task`) keyed by `subAgentId`, showing its task
 * goal, lifecycle phase badge (已启动 / 执行中 / 已完成 / 失败), role,
 * turn count and estimated cost. Driven by `SubAgentEvent` WS events
 * (see `useStreamChat` → `onSubAgentEvent`).
 *
 * 2026-10-23：整个组件从**全内联样式**重写为 className。
 * 此前它 100% 用 `style={{ … }}` 写布局与配色（display/fontSize/padding/
 * borderRadius/color 全在内联里），后果有三条：
 *   1. 拿不到 :hover / :focus，面板里的行永远是死的；
 *   2. 换主题只能改令牌值，改不了这层写死的 `fontSize: 12.5` 之类；
 *   3. 与全站 `--text-*` 七档、`--radius-*` 四档各走一套，同屏字号不一致。
 * 现在配色走语义类、字号走 --text-*、圆角走 --radius-*，与其余面板同源。
 */

const PHASE_CONFIG: Record<
  SubAgentPhase,
  { label: string; badge: string; icon: typeof Loader2 }
> = {
  spawned: { label: "已启动", badge: "bg-accent-soft text-accent", icon: Bot },
  started: { label: "执行中", badge: "bg-accent-soft text-accent", icon: Loader2 },
  done: { label: COMPLETED, badge: "bg-success-soft text-text-success", icon: CheckCircle2 },
  failed: { label: "失败", badge: "bg-error-soft text-text-error", icon: AlertCircle },
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
    <div className="sub-agent-panel footer-block mb-2.5 mt-2.5 rounded-[var(--radius-lg)] bg-[var(--color-bg-secondary)] px-3 py-2.5">
      <div className="mb-2 flex items-center gap-1.5">
        <Sparkles className="icon-md text-accent" />
        <span className="text-[length:var(--text-ui)] font-semibold text-[var(--color-text-primary)]">
          子代理
        </span>
        <span className="text-[length:var(--text-tiny)] text-[var(--color-text-faint)]">
          {list.length}
        </span>
      </div>
      <div className="flex flex-col gap-2">
        {list.map((sa) => {
          const cfg = PHASE_CONFIG[sa.phase];
          const Icon = cfg.icon;
          return (
            <div key={sa.subAgentId} className="rounded-[var(--radius-md)] bg-[var(--color-bg-tertiary)] px-2.5 py-2">
              <div className="flex items-center justify-between gap-2">
                <span
                  title={sa.task}
                  className="min-w-0 flex-1 truncate text-[length:var(--text-meta)] text-[var(--color-text-primary)]"
                >
                  {sa.task || "（无任务描述）"}
                </span>
                <span
                  className={`inline-flex shrink-0 items-center gap-1 rounded-full px-2 py-0.5 text-[length:var(--text-tiny)] font-semibold ${cfg.badge}`}
                >
                  <Icon className={`icon-sm ${sa.phase === "started" ? "shrink-0 animate-spin" : ""}`} />
                  {cfg.label}
                </span>
              </div>
              <div className="mt-1 flex items-center gap-2.5 text-[length:var(--text-tiny)] text-[var(--color-text-muted)]">
                {sa.role && <span>{ROLE_LABEL[sa.role] ?? sa.role}</span>}
                {typeof sa.turns === "number" && <span>轮次 {sa.turns}</span>}
                {typeof sa.costUsd === "number" && <span>≈ ¥{sa.costUsd.toFixed(4)}</span>}
              </div>
              {/* Rolling step log — one line per internal action (tool call /
                  thinking), newest at the bottom. The backend sends the last N
                  entries (SUB_AGENT_STEP_LOG_CAP = 6); older rows fall off,
                  giving a terminal-style scroll. */}
              {Array.isArray(sa.recentSteps) && sa.recentSteps.length > 0 && (
                <div className="mt-1.5 flex flex-col gap-0.5 pt-1">
                  {sa.recentSteps.map((s, i) => (
                    <div
                      key={`${sa.subAgentId}-step-${i}`}
                      title={s}
                      className={`truncate font-mono text-[length:var(--text-tiny)] leading-snug ${
                        i === sa.recentSteps!.length - 1 && sa.phase === "started"
                          ? "text-accent"
                          : "text-[var(--color-text-faint)]"
                      }`}
                    >
                      {s}
                    </div>
                  ))}
                </div>
              )}
              {sa.phase === "failed" && sa.error && (
                <div
                  title={sa.error}
                  className="mt-1 truncate text-[length:var(--text-tiny)] text-[var(--color-text-error)]"
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
