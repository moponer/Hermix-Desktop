/* ════════════════════════════════════════════════════════════════
   InlineExpertPanel — an inline panel above the textarea for picking
   an expert.

   Shows: search bar, category tabs, expert card grid.
   Selecting an expert calls setExpert + closes the panel.
   "全部专家" link opens the full AgentsView for power users.
   ════════════════════════════════════════════════════════════════ */

import { useState, useMemo, useRef, useEffect } from "react";
import { Search, X, ArrowRight, Check, AlertTriangle } from "lucide-react";
import { clsx } from "clsx";
import {
  EXPERT_CATEGORIES,
  type ExpertDef,
} from "../data/builtinExperts";
import { useExpertCatalog } from "../hooks/useExpert";
import { AgentAvatar } from "./agents/AgentAvatar";
import { toneForCategory } from "../lib/categoryTone";

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

  // Backend-authoritative catalog. `source` distinguishes the live list
  // from the locally-bundled one; the UI surfaces the difference instead
  // of silently degrading (previously this panel always rendered
  // BUILTIN_EXPERTS and never told anyone).
  const { experts, source, loading } = useExpertCatalog();
  // Gate on `!loading` so the banner doesn't flash during the first fetch —
  // it means "we tried and fell back", not "we haven't asked yet".
  const usingFallback = !loading && source === "builtin-fallback";

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
    let list: ExpertDef[] = experts;
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
  }, [activeCategory, search, experts]);

  // Category counts (only non-empty)
  const categoryCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const e of experts) {
      counts[e.category] = (counts[e.category] || 0) + 1;
    }
    return counts;
  }, [experts]);

  return (
    <div
      ref={panelRef}
      /* 无描边：面板本身是 bg-secondary、压在白色对话区上，底色差已经把
         边界说清楚了，底边那条线是重复表达。 */
      className="bg-bg-secondary"
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
          <Search className="icon-md absolute left-2.5 top-1/2 -translate-y-1/2 text-text-faint" />
          <input
            ref={searchRef}
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="搜索专家名称、角色、类别..."
            className="w-full pl-8 pr-3 py-1.5 rounded-lg bg-bg-card text-[length:var(--text-ui)] text-text-primary placeholder-text-faint outline-none focus:border-accent/40 transition-colors"
          />
          {search && (
            <button
              onClick={() => setSearch("")}
              className="absolute right-2 top-1/2 -translate-y-1/2 text-text-faint hover:text-text-muted"
            >
              <X className="icon-sm" />
            </button>
          )}
        </div>
        <button
          onClick={onClose}
          className="p-1.5 rounded-md text-text-muted hover:text-text-primary hover:bg-bg-hover transition-colors"
          title="关闭"
        >
          <X className="icon-md" />
        </button>
      </div>

      {/* Degraded-source banner — visible, never silent. The backend owns
          the expert catalog; when it can't be reached we fall back to the
          bundled list and SAY SO instead of pretending it's the live one. */}
      {usingFallback && (
        <div
          role="status"
          aria-live="polite"
          className="mx-4 mb-1 flex items-center gap-1.5 px-2 py-1 rounded-md bg-bg-tertiary text-[length:var(--text-meta)] text-text-warning"
        >
          <AlertTriangle aria-hidden="true" className="icon-sm shrink-0" />
          <span>未能连接专家服务，当前显示内置清单</span>
        </div>
      )}

      {/* Category tabs */}
      <div className="flex items-center gap-1 px-4 pb-2 overflow-x-auto" style={{ scrollbarWidth: "none" }}>
        <CategoryTab
          active={activeCategory === "全部"}
          onClick={() => setActiveCategory("全部")}
          label="全部"
          count={experts.length}
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
            <p className="text-[length:var(--text-meta)]">没有匹配的专家</p>
            <p className="text-[length:var(--text-meta)] text-text-faint mt-1">试试其他搜索词或分类</p>
          </div>
        ) : (
          <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 lg:grid-cols-5 gap-2">
            {filtered.map((e) => (
              <button
                key={e.id}
                onClick={() => onSelect(e.id)}
                className={clsx(
                  "group flex items-center gap-2 p-2 rounded-lg text-left transition-[color,background-color,border-color,transform] hover:-translate-y-px",
                  currentExpertId === e.id
                    ? "bg-accent/10 ring-1 ring-accent/30"
                    : "bg-bg-card hover:bg-bg-hover hover:shadow-sm",
                )}
              >
                <AgentAvatar name={e.name} tone={toneForCategory(e.category)} size="sm" className="shrink-0" />
                <div className="min-w-0 flex-1">
                  <div className="text-[length:var(--text-meta)] font-medium text-text-primary truncate leading-tight">
                    {e.name}
                  </div>
                  <div className="text-[length:var(--text-meta)] text-text-muted truncate mt-0.5">
                    {e.role}
                  </div>
                </div>
                {currentExpertId === e.id && (
                  <Check className="icon-sm text-accent shrink-0" />
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
            className="flex items-center gap-1.5 text-[length:var(--text-meta)] text-accent hover:text-accent/80 transition-colors"
>
            全部专家
            <ArrowRight className="icon-sm" />
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
        "flex items-center gap-1 px-2.5 py-1 rounded-full text-[length:var(--text-meta)] whitespace-nowrap transition-colors",
        active
          ? "bg-bg-active text-text-primary font-medium"
          : "bg-bg-tertiary text-text-muted hover:text-text-primary",
      )}
    >
      {label}
      <span className={clsx("text-[length:var(--text-meta)]", active ? "text-accent" : "text-text-faint")}>
        {count}
      </span>
    </button>
  );
}
