import { Search } from "lucide-react";
import { FileTypeIcon } from "./FileTypeIcon";
import { clsx } from "clsx";
import type { Artifact, ArtifactKind } from "../utils/artifacts";
import { FILTERS } from "./rightSidebarUtils";

export function ProductsTab({
  artifacts,
  selectedId,
  onSelectArtifact,
  workspaceRoot,
  rawCount,
  filtered,
  query,
  onQueryChange,
  filter,
  onFilterChange,
}: {
  artifacts: Artifact[];
  selectedId: string | null;
  onSelectArtifact: (id: string | null) => void;
  workspaceRoot?: string | null;
  rawCount?: number;
  filtered: Artifact[];
  query: string;
  onQueryChange: (q: string) => void;
  filter: "all" | ArtifactKind;
  onFilterChange: (f: "all" | ArtifactKind) => void;
}) {
  return (
    <div className="flex-1 flex flex-col overflow-hidden min-h-0">
      {/* Search + filter */}
      <div className="px-3 pb-3 space-y-2 mt-1">
        <div className="flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-bg-tertiary focus-within:ring-1 focus-within:ring-accent/30 transition">
          <Search size={12} className="text-text-muted shrink-0" />
          <input
            value={query}
            onChange={(e) => onQueryChange(e.target.value)}
            placeholder="搜索文件名或路径…"
            className="flex-1 bg-transparent text-[12.5px] text-text-primary placeholder:text-text-faint outline-none min-w-0"
          />
        </div>
        <div className="flex items-center gap-1 flex-wrap">
          {FILTERS.map((f) => {
            const Icon = f.icon;
            const active = filter === f.key;
            return (
              <button
                key={f.key}
                onClick={() => onFilterChange(f.key)}
                className={clsx(
                  "flex items-center gap-1 text-[11.5px] px-2 py-1 rounded-md transition-colors",
                  active
                    ? "bg-accent-soft text-accent font-semibold"
                    : "text-text-muted hover:bg-bg-hover hover:text-text-secondary",
                )}
              >
                <Icon size={10} />
                {f.label}
              </button>
            );
          })}
        </div>
      </div>

      {/* File list */}
      <div className="flex-1 overflow-y-auto border-t border-border-subtle px-3 py-2 min-h-0">
        {filtered.length === 0 ? (
          <div className="px-1 py-8 text-center">
            <p className="h-meta text-text-faint leading-relaxed whitespace-pre-line">
              {artifacts.length === 0
                ? "尚无产物\n对话中工具调用、文件路径会自动出现"
                : "无匹配结果"}
            </p>
            {artifacts.length === 0 && typeof window !== "undefined"
              && window.localStorage?.getItem("hermixDebugArtifacts") === "1" && (
              <p className="mt-3 text-[10.5px] text-text-faint font-mono leading-relaxed">
                raw={rawCount ?? artifacts.length} · verified={artifacts.length}
                <br />
                workspaceRoot={workspaceRoot ?? "(null)"}
              </p>
            )}
          </div>
        ) : (
          <div className="space-y-1.5">
            {filtered.map((a) => {
              const isSelected = selectedId === a.id;
              return (
              <button
                key={a.id}
                onClick={() => onSelectArtifact(a.id)}
                title={a.path || a.name}
                className={clsx(
                    "artifact-card w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-left transition-colors min-w-0",
                    isSelected ? "is-active" : "",
                  )}
              >
                <span className="shrink-0" style={{ display: "inline-flex" }}>
                  <FileTypeIcon name={a.name} size={14} />
                </span>
                <span className="text-[12.5px] text-text-primary truncate leading-tight flex-1 min-w-0">
                  {a.name}
                </span>
              </button>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
