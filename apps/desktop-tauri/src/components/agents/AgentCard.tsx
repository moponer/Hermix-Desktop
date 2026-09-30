import { Play, Edit3, Trash2 } from "lucide-react";
import type { AgentPersona } from "./types";

interface AgentCardProps {
  agent: AgentPersona;
  onEdit: () => void;
  onDelete: () => void;
  onTest: () => void;
}

export function AgentCard({ agent, onEdit, onDelete, onTest }: AgentCardProps) {
  const visibleSkills = agent.skills.slice(0, 3);
  // 右上小标签:按使用次数分级
  const label =
    agent.usageCount === 0 ? "新" :
    agent.usageCount < 4   ? "新晋" :
    agent.usageCount < 10  ? "常用" : "推荐";

  return (
    <div className="group relative rounded-xl bg-bg-card p-3.5 transition-[color,background-color,border-color,transform,box-shadow] hover:-translate-y-0.5 hover:shadow-md">
      <div className="flex items-start gap-2.5">
        {/* Round avatar */}
        <div className="w-9 h-9 rounded-full flex items-center justify-center text-[length:var(--text-display)] shrink-0 bg-bg-tertiary">
          {agent.avatar}
        </div>

        {/* Content */}
        <div className="min-w-0 flex-1">
          <div className="flex items-start justify-between gap-2 mb-0.5">
            <div className="min-w-0 flex-1">
              <h3 className="text-[length:var(--text-ui)] font-semibold text-text-primary truncate leading-tight">
                {agent.name}
              </h3>
              <p className="text-[length:var(--text-meta)] text-text-muted truncate mt-0.5">
                {agent.role || "AI 助手"}
              </p>
            </div>
            <span
              className="text-[length:var(--text-meta)] font-medium px-1.5 py-0.5 rounded shrink-0 mt-0.5 bg-accent-soft text-accent"
            >
              {label}
            </span>
          </div>

          {agent.identity && (
            <p className="text-[length:var(--text-meta)] text-text-secondary leading-relaxed line-clamp-2 mt-1 min-h-[2.4em]">
              {agent.identity}
            </p>
          )}

          {(visibleSkills.length > 0 || agent.specialty) && (
            <div className="flex flex-wrap gap-1 mt-2">
              {visibleSkills.map((s) => (
                <span
                  key={s}
                  className="px-1.5 py-0.5 rounded text-[length:var(--text-meta)] text-text-secondary bg-bg-tertiary"
                >
                  {s}
                </span>
              ))}
              {visibleSkills.length === 0 && agent.specialty && (
                <span className="px-1.5 py-0.5 rounded text-[length:var(--text-meta)] text-text-secondary bg-bg-tertiary">
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
          onClick={onTest}
          title="快速测试"
          className="flex-1 flex items-center justify-center gap-1.5 py-1.5 rounded-lg text-[length:var(--text-meta)] font-medium transition-[color,background-color,border-color,opacity] hover:opacity-90"
          style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}
        >
          <Play className="icon-sm" />
          测试
        </button>
        <button
          onClick={onEdit}
          title="编辑"
          className="p-1.5 rounded-lg hover:bg-bg-hover"
          style={{ color: "var(--color-btn-ghost-text)" }}
          aria-label="编辑">
          <Edit3 className="icon-sm" />
        </button>
        <button
          onClick={onDelete}
          title="删除"
          className="p-1.5 rounded-lg hover:bg-error-soft"
          style={{ color: "var(--color-btn-ghost-text)" }}
          aria-label="删除">
          <Trash2 className="icon-sm" />
        </button>
      </div>
    </div>
  );
}
