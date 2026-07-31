/* ════════════════════════════════════════════════════════════════
   InlineExpertPanel — an inline panel above the textarea for picking
   an expert.

   Shows: search bar, category tabs, expert card grid.
   Selecting an expert calls setExpert + closes the panel.
   "全部专家" link opens the full AgentsView for power users.
   ════════════════════════════════════════════════════════════════ */

import { useState, useMemo, useRef, useEffect } from "react";
import { Search, X, ArrowRight, Check } from "lucide-react";
import { clsx } from "clsx";
import {
  BUILTIN_EXPERTS,
  EXPERT_CATEGORIES,
  type ExpertDef,
} from "../data/builtinExperts";

interface InlineExpertPanelProps {
  currentExpertId: string | null;
  onSelect: (id: string) => void;
  onClose: () => void;
  onOpenFullCatalog?: () => void;
}

export function InlineExpertPanel({
  currentExpertId,
  onSelect,
  onClose,
  onOpenFullCatalog,
}: InlineExpertPanelProps) {
  const [search, setSearch] = useState("");
  const [activeCategory, setActiveCategory] = useState<string>("全部");
  const searchRef = useRef<HTMLInputElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // Auto-focus search on mount
  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  // Close on Escape
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [onClose]);

  // Filter experts
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

  // Category counts (only non-empty)
  const categoryCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const e of BUILTIN_EXPERTS) {
      counts[e.category] = (counts[e.category] || 0) + 1;
    }
    return counts;
  }, []);

  return (
    <div
      ref={panelRef}
      className="border-b border-border-subtle bg-bg-secondary"
      style={{ animation: "slideDown 0.15s ease-out" }}
    >
      <style>{`
        @keyframes slideDown {
          from { opacity: 0; max-height: 0; }
          to { opacity: 1; max-height: 400px; }
        }
      `}</style>

      {/* Search + close row */}
      <div className="flex items-center gap-2 px-4 pt-3 pb-2">
        <div className="relative flex-1">
          <Search size={13} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-text-faint" />
          <input
            ref={searchRef}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="搜索专家名称、角色、类别..."
            className="w-full pl-8 pr-3 py-1.5 rounded-lg bg-bg-card text-[12.5px] text-text-primary placeholder-text-faint outline-none border border-border-subtle focus:border-accent/40 transition-colors"
          />
          {search && (
            <button
              onClick={() => setSearch("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-text-faint hover:text-text-muted"
            >
              <X size={12} />
            </button>
          )}
        </div>
        <button
          onClick={onClose}
          className="p-1.5 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-hover transition-colors"
          title="关闭"
        >
          <X size={14} />
        </button>
      </div>

      {/* Category tabs */}
      <div className="flex items-center gap-1 px-4 pb-2 overflow-x-auto" style={{ scrollbarWidth: "none" }}>
        <CategoryTab
          active={activeCategory === "全部"}
          onClick={() => setActiveCategory("全部")}
          label="全部"
          count={BUILTIN_EXPERTS.length}
        />
        {EXPERT_CATEGORIES.map((cat) => {
          const count = categoryCounts[cat];
          if (!count) return null;
          return (
            <CategoryTab
              key={cat}
              active={activeCategory === cat}
              onClick={() => setActiveCategory(cat)}
              label={cat}
              count={count}
            />
          );
        })}
      </div>

      {/* Expert grid */}
      <div className="px-4 pb-3 max-h-[240px] overflow-y-auto">
        {filtered.length === 0 ? (
          <div className="flex flex-col items-center py-6 text-center text-text-muted">
            <p className="text-[12px]">没有匹配的专家</p>
            <p className="text-[11px] text-text-faint mt-1">试试其他搜索词或分类</p>
          </div>
        ) : (
          <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-2">
            {filtered.map((e) => (
              <button
                key={e.id}
                onClick={() => onSelect(e.id)}
                className={clsx(
                  "group flex items-center gap-2 p-2 rounded-lg text-left transition-all hover:-translate-y-px",
                  currentExpertId === e.id
                    ? "bg-accent/10 ring-1 ring-accent/30"
                    : "bg-bg-card hover:bg-bg-hover hover:shadow-sm",
                )}
              >
                <div className="w-8 h-8 rounded-lg bg-bg-tertiary flex items-center justify-center text-[16px] shrink-0 group-hover:bg-accent/15 transition-colors">
                  {e.avatar}
                </div>
                <div className="min-w-0 flex-1">
                  <div className="text-[11.5px] font-medium text-text-primary truncate leading-tight">
                    {e.name}
                  </div>
                  <div className="text-[10px] text-text-muted truncate mt-0.5">
                    {e.role}
                  </div>
                </div>
                {currentExpertId === e.id && (
                  <Check size={12} className="text-accent shrink-0" />
                )}
              </button>
            ))}
          </div>
        )}
      </div>

      {/* Footer: "全部专家" link */}
      {onOpenFullCatalog && (
        <div className="px-4 pb-2.5">
          <button
            onClick={() => { onOpenFullCatalog(); onClose(); }}
            className="flex items-center gap-1.5 text-[11px] text-accent hover:text-accent/80 transition-colors"
          >
            全部专家
            <ArrowRight size={11} />
          </button>
        </div>
      )}
    </div>
  );
}

function CategoryTab({
  active,
  onClick,
  label,
  count,
}: {
  active: boolean;
  onClick: () => void;
  label: string;
  count: number;
}) {
  return (
    <button
      onClick={onClick}
      className={clsx(
        "flex items-center gap-1 px-2.5 py-1 rounded-full text-[11px] whitespace-nowrap transition-colors",
        active
          ? "bg-accent text-white"
          : "bg-bg-tertiary text-text-muted hover:text-text-primary",
      )}
    >
      {label}
      <span className={clsx("text-[9.5px]", active ? "text-white/70" : "text-text-faint")}>
        {count}
      </span>
    </button>
  );
}
