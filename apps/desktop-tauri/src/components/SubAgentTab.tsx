// components/SubAgentTab.tsx
//
// 「辅助对话」视图 —— 展开被 `delegate_task` 派出去的子代理：任务目标、
// 生命周期、轮次、成本，以及它最近做了什么（recentSteps）。
//
// 为什么要它：子代理的中间过程**不进主对话上下文**（这是 context-bloat
// 隔离的刻意设计，见 AGENTS.md「零上下文膨胀」）。所以主对话里只能看到
// 一句"已委派"，看不到它到底干了什么。这个标签把那层隔离打开给用户看 ——
// 只看，不参与后续推理。
//
// 与 `SubAgentPanel.tsx`（对话底部的一行状态条）的分工：那边只回答
// "有没有在跑"，这里回答"跑了什么、结果如何"。相位标签与配色共用同一套
// PHASE_CONFIG 定义，避免两处漂移。
//
// **能力边界**：`SubAgentEvent` 只带 `recentSteps`（工具调用/思考的单行
// 摘要滚动日志），**不带子代理的完整消息流**。所以这是"过程日志"而非
// "可继续追问的独立会话"——真要能在侧栏里接着和某个子代理对话，需要后端
// 保留子代理的完整消息并提供 resume 通道，目前没有。

import { Bot, Sparkles, Loader2, CheckCircle2, AlertCircle, ChevronRight } from "lucide-react";
import { clsx } from "clsx";
import type { SubAgentPhase, SubAgentState } from "../types";
import { COMPLETED } from "./strings";

const PHASE_CONFIG: Record<SubAgentPhase, { label: string; Icon: typeof Loader2 }> = {
  spawned: { label: "已启动", Icon: Bot },
  started: { label: "执行中", Icon: Loader2 },
  done: { label: COMPLETED, Icon: CheckCircle2 },
  failed: { label: "失败", Icon: AlertCircle },
};

const ROLE_LABEL: Record<string, string> = {
  general: "通用",
  explore: "探索",
  plan: "规划",
};

export function SubAgentTab({ subAgents }: { subAgents: Record<string, SubAgentState> }) {
  const list = Object.values(subAgents).sort((a, b) => b.updatedAt - a.updatedAt);

  if (list.length === 0) {
    return (
      <div className="subagent-empty">
        <Sparkles className="icon-lg" />
        <p className="subagent-empty__title">本会话没有子代理</p>
      </div>
    );
  }

  const running = list.filter((s) => s.phase === "started" || s.phase === "spawned").length;

  return (
    <div className="subagent-list">
      <div className="subagent-list__head">
        <span>{list.length} 个子代理</span>
        <span className="cs-spacer" />
        {running > 0 && <span className="subagent-list__running">{running} 个进行中</span>}
      </div>

      {list.map((sa) => {
        const cfg = PHASE_CONFIG[sa.phase];
        const steps = sa.recentSteps ?? [];
        return (
          <details key={sa.subAgentId} className="subagent-card" open={sa.phase === "failed"}>
            <summary className="subagent-card__head">
              <span className={clsx("subagent-card__phase", `is-${sa.phase}`)}>
                <cfg.Icon
                  className={clsx("icon-sm shrink-0", sa.phase === "started" && "animate-spin")}
                />
                {cfg.label}
              </span>
              <span className="subagent-card__task" title={sa.task}>
                {sa.task || "（无任务描述）"}
              </span>
              <ChevronRight className="subagent-card__chevron icon-sm" />
            </summary>

            <div className="subagent-card__meta">
              {sa.role && <span>{ROLE_LABEL[sa.role] ?? sa.role}</span>}
              {typeof sa.turns === "number" && <span>轮次 {sa.turns}</span>}
              {typeof sa.costUsd === "number" && <span>≈ ¥{sa.costUsd.toFixed(4)}</span>}
            </div>

            {sa.error && (
              <div className="subagent-card__error">
                <AlertCircle className="icon-sm shrink-0" />
                <span>{sa.error}</span>
              </div>
            )}

            {steps.length > 0 && (
              <ol className="subagent-steps">
                {steps.map((s, i) => (
                  <li key={`${sa.subAgentId}-step-${i}`} className="subagent-step">
                    <span className="subagent-step__idx">{i + 1}</span>
                    <span className="subagent-step__text">{s}</span>
                  </li>
                ))}
              </ol>
            )}
          </details>
        );
      })}
    </div>
  );
}
