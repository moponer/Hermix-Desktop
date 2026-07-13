/* ════════════════════════════════════════════════════════════════
   Agent Card v3 — 参考图风格:大圆形头像 + 名字/角色 + 描述 + chips + 行动按钮
   ════════════════════════════════════════════════════════════════ */

import { MessageSquare, Play, Edit3, Trash2 } from "lucide-react";
import type { AgentPersona } from "./types";

interface AgentCardProps {
  agent: AgentPersona;
  onEdit: () => void;
  onDelete: () => void;
  onTest: () => void;
  onUse: () => void;
}

export function AgentCard({ agent, onEdit, onDelete, onTest, onUse }: AgentCardProps) {
  const visibleSkills = agent.skills.slice(0, 3);
  // 右上小标签:按使用次数分级
  const label =
    agent.usageCount === 0 ? "新" :
    agent.usageCount < 4   ? "新晋" :
    agent.usageCount < 10  ? "常用" : "推荐";

  return (
    <div className="group relative rounded-xl bg-bg-card p-3.5 transition-all hover:-translate-y-0.5 hover:shadow-md">
      <div className="flex items-start gap-2.5">
        {/* Round avatar */}
        <div className="w-9 h-9 rounded-full flex items-center justify-center text-[20px] shrink-0 bg-bg-tertiary">
          {agent.avatar}
        </div>

        {/* Content */}
        <div className="min-w-0 flex-1">
          <div className="flex items-start justify-between gap-2 mb-0.5">
            <div className="min-w-0 flex-1">
              <h3 className="text-[12.5px] font-semibold text-text-primary truncate leading-tight">
                {agent.name}
              </h3>
              <p className="text-[10.5px] text-text-muted truncate mt-0.5">
                {agent.role || "AI 助手"}
              </p>
            </div>
            <span
              className="text-[9.5px] font-medium px-1.5 py-0.5 rounded shrink-0 mt-0.5 bg-accent-soft text-accent"
            >
              {label}
            </span>
          </div>

          {agent.identity && (
            <p className="text-[11px] text-text-secondary leading-relaxed line-clamp-2 mt-1 min-h-[2.4em]">
              {agent.identity}
            </p>
          )}

          {(visibleSkills.length > 0 || agent.specialty) && (
            <div className="flex flex-wrap gap-1 mt-2">
              {visibleSkills.map((s) => (
                <span
                  key={s}
                  className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary bg-bg-tertiary"
                >
                  {s}
                </span>
              ))}
              {visibleSkills.length === 0 && agent.specialty && (
                <span className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary bg-bg-tertiary">
                  {agent.specialty}
                </span>
              )}
            </div>
          )}
        </div>
      </div>

      {/* Actions — no border-top, just gap */}
      <div className="flex items-center gap-1.5 mt-2.5">
        <button
          onClick={onUse}
          className="flex-1 flex items-center justify-center gap-1.5 py-1.5 rounded-lg text-[11.5px] font-medium transition-all hover:opacity-90"
          style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
        >
          <MessageSquare size={11} />
          在对话中使用
        </button>
        <button
          onClick={onTest}
          title="快速测试"
          className="p-1.5 rounded-lg hover:bg-bg-hover"
          style={{ color: "var(--color-btn-ghost-text)" }}
         aria-label="快速测试">
          <Play size={11} />
        </button>
        <button
          onClick={onEdit}
          title="编辑"
          className="p-1.5 rounded-lg hover:bg-bg-hover"
          style={{ color: "var(--color-btn-ghost-text)" }}
         aria-label="编辑">
          <Edit3 size={11} />
        </button>
        <button
          onClick={onDelete}
          title="删除"
          className="p-1.5 rounded-lg hover:bg-error-soft"
          style={{ color: "var(--color-btn-ghost-text)" }}
         aria-label="删除">
          <Trash2 size={11} />
        </button>
      </div>
    </div>
  );
}