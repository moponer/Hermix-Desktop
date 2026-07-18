/* ════════════════════════════════════════════════════════════════
   Built-in Experts tab — search/filter the catalog of pre-shipped
   agent personas (product, tech, finance, marketing, content, etc.)

   This is a read-only catalog view. Expert selection happens in the
   chat composer's inline panel, not here.
   ════════════════════════════════════════════════════════════════ */

import { useState, useMemo } from "react";
import { Bot } from "lucide-react";
import { clsx } from "clsx";
import { BUILTIN_EXPERTS, EXPERT_CATEGORIES, type ExpertDef } from "../../data/builtinExperts";

export function BuiltInExpertsTab() {
  const [search, setSearch] = useState("");
  const [activeCategory, setActiveCategory] = useState<string>("全部");

  const filtered = useMemo(() => {
    let list: ExpertDef[] = BUILTIN_EXPERTS;
    if (activeCategory !== "全部") {
      list = list.filter((e) => e.category === activeCategory);
    }
    if (search.trim()) {
      const q = search.trim().toLowerCase();
      list = list.filter(
        (e) =>
          e.name.toLowerCase().includes(q) ||
          e.role.toLowerCase().includes(q) ||
          e.description.toLowerCase().includes(q) ||
          e.category.toLowerCase().includes(q),
      );
    }
    return list;
  }, [activeCategory, search]);

  return (
    <div className="p-6">
      <div className="max-w-7xl mx-auto">
        <p className="text-[12.5px] text-text-muted mb-4">
          Hermix 内置的专家角色 — 涵盖产品、技术、金融、营销、内容创作等领域。在对话界面点击"专家"选择使用。
        </p>
        <div className="flex items-center gap-2 mb-3">
          <input
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="搜索专家、角色、类别..."
            className="flex-1 px-3 py-1.5 rounded-md bg-bg-card text-[12.5px] text-text-primary placeholder-text-faint outline-none"
          />
        </div>
        <div className="flex items-center gap-1.5 mb-5 overflow-x-auto" style={{ scrollbarWidth: "none" }}>
          <CategoryChip
            active={activeCategory === "全部"}
            onClick={() => setActiveCategory("全部")}
            label="全部"
            count={BUILTIN_EXPERTS.length}
          />
          {EXPERT_CATEGORIES.map((cat) => {
            const count = BUILTIN_EXPERTS.filter((e) => e.category === cat).length;
            if (count === 0) return null;
            return (
              <CategoryChip
                key={cat}
                active={activeCategory === cat}
                onClick={() => setActiveCategory(cat)}
                label={cat}
                count={count}
              />
            );
          })}
        </div>
        {filtered.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-20 text-center text-text-muted">
            <Bot size={36} className="mb-3 opacity-40" />
            <p className="text-sm">没有匹配的内置专家</p>
            <p className="text-[11px] text-text-faint mt-1">试试其他类别或搜索词</p>
          </div>
        ) : (
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-4 gap-3">
            {filtered.map((e) => (
              <ExpertCard key={e.id} expert={e} />
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

function CategoryChip({
  active, onClick, label, count,
}: { active: boolean; onClick: () => void; label: string; count: number }) {
  return (
    <button
      onClick={onClick}
      className={clsx(
        "flex items-center gap-1 px-2.5 py-1 rounded-full text-[11.5px] whitespace-nowrap",
        active
          ? "bg-accent text-white"
          : "bg-bg-tertiary text-text-muted hover:text-text-primary",
      )}
    >
      {label}
      <span className={clsx("text-[10px]", active ? "text-white/80" : "text-text-faint")}>{count}</span>
    </button>
  );
}

function ExpertCard({ expert }: { expert: ExpertDef }) {
  return (
    <div className="rounded-xl bg-bg-card p-3.5 transition-all hover:-translate-y-0.5 hover:shadow-md">
      <div className="flex items-start gap-2.5">
        <div className="w-9 h-9 rounded-full flex items-center justify-center text-[20px] shrink-0 bg-bg-tertiary">
          {expert.avatar}
        </div>
        <div className="min-w-0 flex-1">
          <div className="mb-0.5">
            <h3 className="text-[12.5px] font-semibold text-text-primary truncate leading-tight">
              {expert.name}
            </h3>
            <p className="text-[10.5px] text-text-muted truncate mt-0.5">
              {expert.role}
            </p>
          </div>
          <p className="text-[11px] text-text-secondary leading-relaxed line-clamp-2 mt-1 min-h-[2.4em]">
            {expert.description}
          </p>
          <div className="flex flex-wrap gap-1 mt-2">
            <span className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary bg-bg-tertiary">
              {expert.category}
            </span>
            {expert.toolset && (
              <span className="px-1.5 py-0.5 rounded text-[10px] text-text-secondary bg-bg-tertiary">
                {expert.toolset}
              </span>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
