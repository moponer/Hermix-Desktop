import { useEffect, useRef, useState, useCallback, useMemo } from "react";
import { createPortal } from "react-dom";
import {
  Folder, ArrowLeft, ArrowRight, ArrowUp,
  RefreshCw, List, FileText, LayoutGrid, Loader2,
  AlertTriangle, FolderOpen, ChevronRight,
} from "lucide-react";
import { FileTypeIcon } from "./FileTypeIcon";
import { clsx } from "clsx";
import { openExternalUrl, listDirectory, type DirEntry } from "../desktopBridge";
import { homeDir } from "@tauri-apps/api/path";
import { FileExplorerContextMenu } from "./FileContextMenu";
import { formatBytes, formatModifiedDate, pathToBreadcrumbs } from "./rightSidebarUtils";

type ViewMode = "list" | "detail" | "icons";

export function ComputerTab({
  workspaceRoot,
  onAddToChat,
  onPreviewFile,
}: {
  workspaceRoot?: string | null;
  onAddToChat?: (path: string) => void;
  onPreviewFile?: (path: string, name: string) => void;
}) {
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [currentPath, setCurrentPath] = useState<string>("");
  const [rootPath, setRootPath] = useState<string>(workspaceRoot || "");
  const [viewMode, setViewMode] = useState<ViewMode>("list");
  const [historyStack, setHistoryStack] = useState<string[]>([]);
  const [forwardStack, setForwardStack] = useState<string[]>([]);
  const [selectedEntry, setSelectedEntry] = useState<string | null>(null);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number; entry: DirEntry } | null>(null);
  const scrollRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (workspaceRoot) { setRootPath(workspaceRoot); return; }
    homeDir().then(setRootPath).catch(() => setRootPath(""));
  }, [workspaceRoot]);

  const loadDirectory = useCallback(async (path: string) => {
    if (!path) return;
    setLoading(true);
    setError(null);
    try {
      const result = await listDirectory(path, workspaceRoot);
      setEntries(result);
      setCurrentPath(path);
    } catch (e) {
      setError(String(e).replace(/^"|"$/g, ""));
    } finally {
      setLoading(false);
    }
  }, [workspaceRoot]);

  useEffect(() => {
    if (!rootPath) return;
    loadDirectory(rootPath);
  }, [rootPath, loadDirectory]);

  const navigateTo = useCallback((path: string) => {
    if (!path) return;
    if (currentPath && path !== currentPath) {
      setHistoryStack((prev) => [...prev, currentPath]);
      setForwardStack([]);
    }
    loadDirectory(path);
    scrollRef.current?.scrollTo({ top: 0 });
  }, [currentPath, loadDirectory]);

  const navigateBack = useCallback(() => {
    setHistoryStack((prev) => {
      if (prev.length === 0) return prev;
      const last = prev[prev.length - 1];
      setCurrentPath((cur) => {
        if (cur && cur !== last) {
          setForwardStack((fwd) => [...fwd, cur]);
        }
        return last;
      });
      loadDirectory(last);
      return prev.slice(0, -1);
    });
  }, [loadDirectory]);

  const navigateForward = useCallback(() => {
    setForwardStack((prev) => {
      if (prev.length === 0) return prev;
      const last = prev[prev.length - 1];
      setCurrentPath((cur) => {
        if (cur && cur !== last) {
          setHistoryStack((back) => [...back, cur]);
        }
        return last;
      });
      loadDirectory(last);
      return prev.slice(0, -1);
    });
  }, [loadDirectory]);

  const navigateUp = useCallback(() => {
    if (!currentPath) return;
    let parent: string;
    if (/^[A-Za-z]:[\\/]?$/.test(currentPath)) {
      return;
    }
    if (currentPath === "/" || currentPath === "\\") {
      return;
    }
    parent = currentPath.replace(/[\\/][^\\/]+$/, "") || currentPath;
    if (/^[A-Za-z]:$/.test(parent)) parent = parent + "\\";
    if (parent && parent !== currentPath) {
      navigateTo(parent);
    }
  }, [currentPath, navigateTo]);

  const handleEntryClick = useCallback((entry: DirEntry) => {
    if (entry.isDir) {
      navigateTo(entry.path);
    } else {
      onPreviewFile?.(entry.path, entry.name);
    }
  }, [navigateTo, onPreviewFile]);

  const handleEntryDoubleClick = useCallback((entry: DirEntry) => {
    if (entry.isDir) {
      navigateTo(entry.path);
    }
  }, [navigateTo]);

  const handleContextMenu = useCallback((e: React.MouseEvent, entry: DirEntry) => {
    e.preventDefault();
    e.stopPropagation();
    setSelectedEntry(entry.path);
    setContextMenu({ x: e.clientX, y: e.clientY, entry });
  }, []);

  const handleMenuAction = useCallback((action: string) => {
    if (!contextMenu) return;
    const entry = contextMenu.entry;
    setContextMenu(null);
    switch (action) {
      case "open":
        if (entry.isDir) navigateTo(entry.path);
        else onPreviewFile?.(entry.path, entry.name);
        break;
      case "add_to_chat":
        if (!entry.isDir) onAddToChat?.(entry.path);
        break;
      case "preview":
        if (!entry.isDir) onPreviewFile?.(entry.path, entry.name);
        break;
      case "copy_path":
        navigator.clipboard.writeText(entry.path);
        break;
      case "open_in_explorer":
        openExternalUrl(entry.isDir ? entry.path : entry.path.replace(/[\\/][^\\/]+$/, ""));
        break;
      case "open_external":
        openExternalUrl(entry.path);
        break;
    }
  }, [contextMenu, navigateTo, onAddToChat, onPreviewFile]);

  useEffect(() => {
    if (!contextMenu) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setContextMenu(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [contextMenu]);

  const breadcrumbs = useMemo(() => pathToBreadcrumbs(currentPath), [currentPath]);

  return (
    <div className="flex-1 flex flex-col overflow-hidden min-h-0 computer-tab">
      <div className="computer-toolbar">
        <div className="computer-toolbar-left">
          <button
            type="button"
            onClick={navigateBack}
            disabled={historyStack.length === 0}
            title="后退"
            className="computer-nav-btn"
            aria-label="后退"
          >
            <ArrowLeft className="icon-md" />
          </button>
          <button
            type="button"
            onClick={navigateForward}
            disabled={forwardStack.length === 0}
            title="前进"
            className="computer-nav-btn"
            aria-label="前进"
          >
            <ArrowRight className="icon-md" />
          </button>
          <button
            type="button"
            onClick={navigateUp}
            disabled={!currentPath}
            title="上一级"
            className="computer-nav-btn"
            aria-label="上一级"
          >
            <ArrowUp className="icon-md" />
          </button>
          <button
            type="button"
            onClick={() => currentPath && loadDirectory(currentPath)}
            disabled={!currentPath || loading}
            title="刷新"
            className="computer-nav-btn"
            aria-label="刷新"
          >
            <RefreshCw className={`icon-md ${loading ? "animate-spin" : ""}`} />
          </button>
        </div>

        <div className="computer-addressbar">
          {breadcrumbs.length === 0 && (
            <span className="text-text-faint text-[length:var(--text-meta)]">选择目录…</span>
          )}
          {breadcrumbs.map((seg, i) => (
            <span key={`crumb-${seg.label || i}`} className="computer-crumb-group">
              <button
                type="button"
                onClick={() => navigateTo(seg.path)}
                className="computer-crumb"
                title={seg.path}
              >
                {seg.label}
              </button>
              {i < breadcrumbs.length - 1 && (
                <ChevronRight size={10} className="computer-crumb-sep" />
              )}
            </span>
          ))}
        </div>

        <div className="computer-view-toggle">
          <button
            type="button"
            onClick={() => setViewMode("list")}
            className={clsx("computer-view-btn", viewMode === "list" && "is-active")}
            title="列表视图"
            aria-label="列表视图"
          >
            <List className="icon-md" />
          </button>
          <button
            type="button"
            onClick={() => setViewMode("detail")}
            className={clsx("computer-view-btn", viewMode === "detail" && "is-active")}
            title="详细信息"
            aria-label="详细信息"
          >
            <FileText className="icon-md" />
          </button>
          <button
            type="button"
            onClick={() => setViewMode("icons")}
            className={clsx("computer-view-btn", viewMode === "icons" && "is-active")}
            title="大图标视图"
            aria-label="大图标视图"
          >
            <LayoutGrid className="icon-md" />
          </button>
        </div>
      </div>

      <div ref={scrollRef} className="flex-1 overflow-y-auto computer-body min-h-0">
        {loading && (
          <div className="flex items-center gap-2 px-3 py-8 justify-center text-xs text-text-muted">
            <Loader2 className="icon-md animate-spin" />
            加载中…
          </div>
        )}
        {error && (
          <div className="flex items-start gap-2 px-3 py-4 text-xs text-error">
            <AlertTriangle className="icon-md shrink-0 mt-0.5" />
            <span className="break-words [overflow-wrap:anywhere] whitespace-pre-wrap min-w-0 flex-1">{error}</span>
          </div>
        )}
        {!loading && !error && entries.length === 0 && (
          <div className="flex flex-col items-center justify-center py-12 gap-2 text-text-muted">
            <FolderOpen size={28} className="opacity-40" />
            <span className="text-xs">空目录</span>
          </div>
        )}

        {!loading && !error && viewMode === "list" && entries.length > 0 && (
          <div className="computer-list-view">
            {entries.map((entry) => (
              <div
                key={entry.path}
                onClick={() => handleEntryClick(entry)}
                onDoubleClick={() => handleEntryDoubleClick(entry)}
                onContextMenu={(e) => handleContextMenu(e, entry)}
                className={clsx(
                  "computer-list-row",
                  selectedEntry === entry.path && "is-selected",
                )}
              >
                <span className="computer-list-icon">
                  {entry.isDir ? (
                    <Folder className="icon-md text-accent" />
                  ) : (
                    <FileTypeIcon name={entry.name} className="icon-md" />
                  )}
                </span>
                <span className="computer-list-name truncate">{entry.name}</span>
              </div>
            ))}
          </div>
        )}

        {!loading && !error && viewMode === "detail" && entries.length > 0 && (
          <div className="computer-detail-view">
            <div className="computer-detail-header">
              <span className="computer-detail-col-name">名称</span>
              <span className="computer-detail-col-size">大小</span>
              <span className="computer-detail-col-date">修改日期</span>
            </div>
            {entries.map((entry) => (
              <div
                key={entry.path}
                onClick={() => handleEntryClick(entry)}
                onDoubleClick={() => handleEntryDoubleClick(entry)}
                onContextMenu={(e) => handleContextMenu(e, entry)}
                className={clsx(
                  "computer-detail-row",
                  selectedEntry === entry.path && "is-selected",
                )}
              >
                <span className="computer-detail-col-name">
                  <span className="computer-list-icon">
                    {entry.isDir ? (
                      <Folder className="icon-md text-accent" />
                    ) : (
                      <FileTypeIcon name={entry.name} className="icon-md" />
                    )}
                  </span>
                  <span className="truncate">{entry.name}</span>
                </span>
                <span className="computer-detail-col-size">
                  {entry.isDir ? "—" : formatBytes(entry.size)}
                </span>
                <span className="computer-detail-col-date">
                  {formatModifiedDate(entry.modifiedMs)}
                </span>
              </div>
            ))}
          </div>
        )}

        {!loading && !error && viewMode === "icons" && entries.length > 0 && (
          <div className="computer-icons-view">
            {entries.map((entry) => (
              <div
                key={entry.path}
                onClick={() => handleEntryClick(entry)}
                onDoubleClick={() => handleEntryDoubleClick(entry)}
                onContextMenu={(e) => handleContextMenu(e, entry)}
                className={clsx(
                  "computer-icon-tile",
                  selectedEntry === entry.path && "is-selected",
                )}
                title={entry.path}
              >
                <span className="computer-icon-img">
                  {entry.isDir ? (
                    <Folder size={32} className="text-accent" />
                  ) : (
                    <FileTypeIcon name={entry.name} size={32} />
                  )}
                </span>
                <span className="computer-icon-label">{entry.name}</span>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="computer-statusbar">
        <span>{entries.length} 项</span>
        {currentPath && (
          <>
            <span className="cs-spacer" />
            <span className="truncate text-text-faint" title={currentPath}>{currentPath}</span>
          </>
        )}
      </div>

      {contextMenu && createPortal(
        <FileExplorerContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          entry={contextMenu.entry}
          onAction={handleMenuAction}
          onClose={() => setContextMenu(null)}
        />,
        document.body,
      )}
    </div>
  );
}
