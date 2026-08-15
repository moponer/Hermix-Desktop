import { useEffect, useMemo, useState } from "react";
import { X, Monitor, Globe, FolderOpen } from "lucide-react";
import { clsx } from "clsx";
import type { Artifact, ArtifactKind } from "../utils/artifacts";
import { ComputerTab } from "./ComputerTab";
import { ProductsTab } from "./ProductsTab";
import { BrowserTab } from "./BrowserTab";
// Re-exported so existing importers (e.g. ArtifactPreviewModal) keep working.
export { PreviewPanel } from "./PreviewPanel";

// ── Tab types ──────────────────────────────────────────
export type RightTab = "computer" | "products" | "browser";

interface RightSidebarProps {
  open: boolean;
  onClose: () => void;
  artifacts: Artifact[];
  selectedId: string | null;
  onSelectArtifact: (id: string | null) => void;
  workspaceRoot?: string | null;
  /** Sync probe count for diagnostic display. App passes the
   * pre-triage raw count so the empty state can hint at "scanned X
   * but filtered all out" vs "didn't find any". */
  rawCount?: number;
  /** Callback when a file is added to chat from the file explorer. */
  onAddFileToChat?: (path: string) => void;
  /** Callback when a file should be previewed from the file explorer. */
  onPreviewFile?: (path: string, name: string) => void;
  /** 允许外部控制当前标签（如点目录 chip 直接切到「电脑」） */
  activeTab?: RightTab;
  onTabChange?: (tab: RightTab) => void;
}

export function RightSidebar({ open, onClose, artifacts, selectedId, onSelectArtifact, workspaceRoot, rawCount, onAddFileToChat, onPreviewFile, activeTab: activeTabProp, onTabChange }: RightSidebarProps) {
  const [internalTab, setInternalTab] = useState<RightTab>("products");
  const activeTab = activeTabProp ?? internalTab;
  const setActiveTab = (t: RightTab) => {
    setInternalTab(t);
    onTabChange?.(t);
  };
  const [filter, setFilter] = useState<"all" | ArtifactKind>("all");
  const [query, setQuery] = useState("");

  const filtered = useMemo(() => {
    return artifacts.filter((a) => {
      if (filter !== "all" && a.kind !== filter) return false;
      if (query) {
        const q = query.toLowerCase();
        if (!a.name.toLowerCase().includes(q) && !a.path.toLowerCase().includes(q)) return false;
      }
      return true;
    });
  }, [artifacts, filter, query]);

  useEffect(() => {
    const selectionStillValid = selectedId && filtered.some((a) => a.id === selectedId);
    if (selectedId && !selectionStillValid) {
      onSelectArtifact(filtered.length > 0 ? filtered[0].id : null);
    }
  }, [selectedId, filtered, onSelectArtifact]);

  if (!open) return null;

  return (
    <aside
      className="flex h-full flex-col flex-1 overflow-hidden"
      style={{ background: "var(--color-bg-primary)" }}
    >
      {/* Tab bar */}
      <div className="flex border-b border-border-primary">
        <TabButton
          active={activeTab === "computer"}
          onClick={() => setActiveTab("computer")}
          icon={<Monitor size={13} />}
          label="电脑"
        />
        <TabButton
          active={activeTab === "products"}
          onClick={() => setActiveTab("products")}
          icon={<FolderOpen size={13} />}
          label="产物"
          badge={artifacts.length > 0 ? artifacts.length : undefined}
        />
        <TabButton
          active={activeTab === "browser"}
          onClick={() => setActiveTab("browser")}
          icon={<Globe size={13} />}
          label="浏览器"
        />
        <button
          onClick={onClose}
          title="关闭右侧栏"
          className="ml-auto p-1.5 m-1 rounded-md hover:bg-bg-hover text-text-muted hover:text-text-primary transition-colors"
         aria-label="关闭右侧栏">
          <X size={13} />
        </button>
      </div>

      {/* Tab content */}
      {activeTab === "computer" && (
        <ComputerTab
          workspaceRoot={workspaceRoot}
          onAddToChat={onAddFileToChat}
          onPreviewFile={onPreviewFile}
        />
      )}

      {activeTab === "products" && (
        <ProductsTab
          artifacts={artifacts}
          selectedId={selectedId}
          onSelectArtifact={onSelectArtifact}
          workspaceRoot={workspaceRoot}
          rawCount={rawCount}
          filtered={filtered}
          query={query}
          onQueryChange={setQuery}
          filter={filter}
          onFilterChange={setFilter}
        />
      )}

      {activeTab === "browser" && <BrowserTab />}
    </aside>
  );
}

// ── Tab Button Component ──────────────────────────────────

function TabButton({
  active,
  onClick,
  icon,
  label,
  badge,
}: {
  active: boolean;
  onClick: () => void;
  icon: React.ReactNode;
  label: string;
  badge?: number;
}) {
  return (
    <button
      onClick={onClick}
      className={clsx(
        "flex items-center gap-1.5 px-3 py-2.5 text-xs font-medium border-b-2 transition-colors",
        active
          ? "border-accent text-accent"
          : "border-transparent text-text-muted hover:text-text-primary hover:border-border-secondary"
      )}
    >
      {icon}
      {label}
      {badge !== undefined && (
        <span className="ml-1 px-1.5 py-0.5 text-[10px] rounded-full bg-accent/20 text-accent">
          {badge}
        </span>
      )}
    </button>
  );
}
