// Hermix 查看器主界面：单 WebView 内 资源管理器 + 多 tab 预览。
//
// 决策笔记 2026-10-13：viewer 一期是「拖入文件 + 多 tab」，二期是
// 「真实资源管理器」。这一版把两侧合并：左侧树 + 中部列表 + 右侧预览，
// 文件双击进入预览 tab（PDF/图片/Office …），新 tab 不会持续再开窗口。
//
// 共享包走 @hermix/preview —— 与桌面 app 用同一份解析器、同一份样式层。

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowLeft, ArrowRight, ArrowUp, RefreshCw, Search, Eye, EyeOff,
  ChevronRight, Folder, FolderOpen, File as FileIcon, FileText,
  HardDrive, Copy, ExternalLink, Sparkles, X,
} from "lucide-react";
import type { DirEntryLike, DriveRootLike } from "@hermix/preview";
import { viewerFileSource } from "./fileSource";
import { AiSidebar } from "./AiSidebar";
import { PreviewHost } from "./PreviewHost";

interface Tab {
  id: number;
  path: string;
  name: string;
}

interface ContextMenu {
  x: number;
  y: number;
  entry: DirEntryLike;
}

let nextTabId = 1;

function splitPath(p: string): string[] {
  return p.split(/[\\/]/).filter(Boolean);
}

function fmtSize(n: number): string {
  if (!n) return "";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

function shortDate(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

export function App() {
  // —— 资源管理器状态 ——
  const [currentPath, setCurrentPath] = useState<string | null>(null);
  const [pathInput, setPathInput] = useState("");
  const [entries, setEntries] = useState<DirEntryLike[]>([]);
  const [drives, setDrives] = useState<DriveRootLike[]>([]);
  const [showHidden, setShowHidden] = useState(false);
  const [searchQ, setSearchQ] = useState("");
  const [searching, setSearching] = useState(false);
  const [searchResults, setSearchResults] = useState<DirEntryLike[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);

  // —— 多 tab + 预览 ——
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTabId, setActiveTabId] = useState<number | null>(null);

  // —— AI 右栏 ——
  const [aiOpen, setAiOpen] = useState(false);
  const [selection, setSelection] = useState("");

  // —— 导航历史 ——
  const [backStack, setBackStack] = useState<string[]>([]);
  const [forwardStack, setForwardStack] = useState<string[]>([]);

  // —— 右键菜单 ——
  const [ctxMenu, setCtxMenu] = useState<ContextMenu | null>(null);

  // —— 拖放 ——
  const [dragOver, setDragOver] = useState(false);
  const openedRef = useRef<Set<string>>(new Set());

  // 启动参数 / 单实例投递（与旧实现一致）
  useEffect(() => {
    void invoke<string[]>("viewer_take_pending").then((paths) =>
      paths.forEach((p) => openPath(p)),
    );
    import("@tauri-apps/api/event").then(({ listen }) =>
      listen<string>("viewer-open-file", (e) => openPath(e.payload)).then((u) => u),
    );
    void viewerFileSource.fs!.drives().then(setDrives);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 当前目录：打开时 stat → 找到默认根（第一个盘符）。
  useEffect(() => {
    if (currentPath !== null) return;
    void (async () => {
      if (!viewerFileSource.fs) return;
      const ds = await viewerFileSource.fs.drives();
      setDrives(ds);
      if (ds.length > 0) setCurrentPath(ds[0].path);
    })();
  }, [currentPath]);

  // 列表加载
  useEffect(() => {
    if (!currentPath) return;
    if (!viewerFileSource.fs) return;
    let cancelled = false;
    (async () => {
      setError(null);
      const r = await viewerFileSource.fs!.listDir(currentPath, showHidden);
      if (cancelled) return;
      if (!r.ok) {
        setError(r.message);
        setEntries([]);
        return;
      }
      setEntries(r.entries);
    })();
    return () => {
      cancelled = true;
    };
  }, [currentPath, showHidden, reloadKey, viewerFileSource]);

  // 选中文本追踪
  useEffect(() => {
    const onSel = () => {
      const sel = window.getSelection()?.toString() ?? "";
      setSelection(sel.length > 8000 ? sel.slice(0, 8000) : sel);
    };
    document.addEventListener("selectionchange", onSel);
    return () => document.removeEventListener("selectionchange", onSel);
  }, []);

  // — 导航 —
  const navigate = useCallback(
    (path: string, record: boolean) => {
      setCurrentPath((cur) => {
        if (record && cur) setBackStack((s) => [...s, cur]);
        setForwardStack([]);
        return path;
      });
      setPathInput(path);
      setSearchQ("");
      setSearchResults([]);
      setSearching(false);
    },
    [],
  );

  const back = useCallback(() => {
    setBackStack((bs) => {
      if (bs.length === 0) return bs;
      const prev = bs[bs.length - 1];
      setForwardStack((f) => [...f, currentPath ?? ""]);
      setCurrentPath(prev);
      setPathInput(prev);
      return bs.slice(0, -1);
    });
  }, [currentPath]);

  const forward = useCallback(() => {
    setForwardStack((fs) => {
      if (fs.length === 0) return fs;
      const next = fs[fs.length - 1];
      setBackStack((b) => [...b, currentPath ?? ""]);
      setCurrentPath(next);
      setPathInput(next);
      return fs.slice(0, -1);
    });
  }, [currentPath]);

  const up = useCallback(async () => {
    if (!currentPath || !viewerFileSource.fs) return;
    const p = await viewerFileSource.fs.parent(currentPath);
    if (p) navigate(p, true);
  }, [currentPath, navigate]);

  const refresh = useCallback(() => setReloadKey((k) => k + 1), []);

  const onPathSubmit = useCallback(
    (e: React.FormEvent) => {
      e.preventDefault();
      const v = pathInput.trim();
      if (v && v !== currentPath) navigate(v, true);
    },
    [pathInput, currentPath, navigate],
  );

  // — 双击：文件夹进、文件开 tab —
  const openEntry = useCallback(
    (e: DirEntryLike) => {
      if (e.isDir) {
        navigate(e.path, true);
        return;
      }
      openPath(e.path);
    },
    [navigate],
  );

  const openPath = useCallback((path: string) => {
    if (!path) return;
    if (openedRef.current.has(path)) {
      setTabs((ts) => {
        const hit = ts.find((t) => t.path === path);
        if (hit) setActiveTabId(hit.id);
        return ts;
      });
      return;
    }
    openedRef.current.add(path);
    const id = nextTabId++;
    const name = path.split(/[\\/]/).pop() ?? path;
    setTabs((ts) => [...ts, { id, path, name }]);
    setActiveTabId(id);
  }, []);

  const closeTab = useCallback((id: number) => {
    setTabs((ts) => {
      const closed = ts.find((t) => t.id === id);
      if (closed) openedRef.current.delete(closed.path);
      const rest = ts.filter((t) => t.id !== id);
      setActiveTabId((cur) =>
        cur === id ? (rest.length ? rest[rest.length - 1].id : null) : cur,
      );
      return rest;
    });
  }, []);

  // 拖放（兼容旧行为）
  const onDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      setDragOver(false);
      for (const f of Array.from(e.dataTransfer.files)) {
        const any = f as File & { path?: string };
        if (any.path) openPath(any.path);
      }
    },
    [openPath],
  );

  // 搜索
  const onSearchSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      if (!currentPath || !viewerFileSource.fs) return;
      const q = searchQ.trim();
      if (!q) {
        setSearchResults([]);
        setSearching(false);
        return;
      }
      setSearching(true);
      const r = await viewerFileSource.fs.search(currentPath, q, showHidden, 200);
      if (r.ok) setSearchResults(r.entries);
      else setError(r.message);
    },
    [searchQ, currentPath, showHidden],
  );

  const exitSearch = useCallback(() => {
    setSearchQ("");
    setSearchResults([]);
    setSearching(false);
  }, []);

  // 右键菜单
  const onContextMenu = useCallback(
    (e: React.MouseEvent, entry: DirEntryLike) => {
      e.preventDefault();
      setCtxMenu({ x: e.clientX, y: e.clientY, entry });
    },
    [],
  );

  useEffect(() => {
    const close = () => setCtxMenu(null);
    if (ctxMenu) {
      document.addEventListener("click", close);
      document.addEventListener("scroll", close, true);
      return () => {
        document.removeEventListener("click", close);
        document.removeEventListener("scroll", close, true);
      };
    }
    return undefined;
  }, [ctxMenu]);

  const copyPath = useCallback((path: string) => {
    void navigator.clipboard?.writeText(path).catch(() => undefined);
  }, []);

  // 面包屑
  const breadcrumbs = useMemo(() => {
    if (!currentPath) return [] as Array<{ label: string; path: string }>;
    const parts = splitPath(currentPath);
    const acc: Array<{ label: string; path: string }> = [];
    let p = "";
    parts.forEach((part, i) => {
      if (i === 0) p = part + (part.endsWith(":") ? "\\" : "");
      else p = p + "\\" + part;
      acc.push({ label: part, path: p });
    });
    return acc;
  }, [currentPath]);

  const active = tabs.find((t) => t.id === activeTabId) ?? null;
  const visibleEntries = searching ? searchResults : entries;

  return (
    <div
      className="app"
      onDragOver={(e) => {
        e.preventDefault();
        setDragOver(true);
      }}
      onDragLeave={() => setDragOver(false)}
      onDrop={onDrop}
    >
      <header className="toolbar">
        <button className="tb-btn" onClick={back} disabled={backStack.length === 0} title="后退">
          <ArrowLeft />
        </button>
        <button
          className="tb-btn"
          onClick={forward}
          disabled={forwardStack.length === 0}
          title="前进"
        >
          <ArrowRight />
        </button>
        <button className="tb-btn" onClick={up} disabled={!currentPath} title="上一级">
          <ArrowUp />
        </button>
        <button className="tb-btn" onClick={refresh} title="刷新">
          <RefreshCw />
        </button>
        <form className="path-form" onSubmit={onPathSubmit}>
          <input
            className="path-input"
            value={pathInput}
            onChange={(e) => setPathInput(e.target.value)}
            placeholder="路径（例如 C:\Users\…）"
          />
          <button type="submit" className="tb-btn" title="跳转">→</button>
        </form>
        <div className="toolbar-spacer" />
        <button
          className={`tb-btn ${showHidden ? "is-active" : ""}`}
          onClick={() => setShowHidden((v) => !v)}
          title="显示隐藏文件"
        >
          {showHidden ? <Eye /> : <EyeOff />}
        </button>
        <button
          className={`tb-btn ${aiOpen ? "is-active" : ""}`}
          onClick={() => setAiOpen((v) => !v)}
          title={aiOpen ? "收起 AI 栏" : "展开 AI 栏"}
        >
          <Sparkles /> AI
        </button>
      </header>

      <nav className="breadcrumb-bar">
        {breadcrumbs.map((b, i) => (
          <span key={i} className="bc-segment">
            <button
              type="button"
              onClick={() => navigate(b.path, true)}
              title={b.path}
              className="bc-btn"
            >
              {b.label}
            </button>
            {i < breadcrumbs.length - 1 && <ChevronRight className="bc-sep" />}
          </span>
        ))}
        {currentPath && (
          <form className="search-form" onSubmit={onSearchSubmit}>
            <Search className="search-icon" />
            <input
              className="search-input"
              value={searchQ}
              onChange={(e) => setSearchQ(e.target.value)}
              placeholder="在当前目录下搜索…"
            />
            {searching && (
              <button type="button" className="search-clear" onClick={exitSearch}>
                <X />
              </button>
            )}
          </form>
        )}
      </nav>

      <div className="body-row">
        <aside className="sidebar">
          <div className="sb-head">本机</div>
          {drives.map((d) => (
            <button
              key={d.path}
              className={`sb-drive ${currentPath === d.path ? "is-active" : ""}`}
              onClick={() => navigate(d.path, true)}
            >
              <HardDrive /> {d.label}
            </button>
          ))}
          <div className="sb-head">书签</div>
          <div className="sb-empty">暂无（即将上线）</div>
        </aside>

        <section className="explorer">
          {error ? (
            <div className="explorer-error">
              <p>{error}</p>
              <small>{currentPath}</small>
            </div>
          ) : (
            <table className="entries">
              <thead>
                <tr>
                  <th>名称</th>
                  <th className="col-size">大小</th>
                  <th className="col-mtime">修改时间</th>
                </tr>
              </thead>
              <tbody>
                {visibleEntries.length === 0 && (
                  <tr>
                    <td colSpan={3} className="empty-cell">
                      {searching ? "无搜索结果" : "空目录"}
                    </td>
                  </tr>
                )}
                {visibleEntries.map((e) => (
                  <tr
                    key={e.path}
                    className={`entry-row ${e.isDir ? "is-dir" : ""}`}
                    onDoubleClick={() => openEntry(e)}
                    onContextMenu={(ev) => onContextMenu(ev, e)}
                    onClick={(ev) => {
                      if (ev.detail === 1) return; // 只响应双击
                    }}
                    title={e.path}
                  >
                    <td className="col-name">
                      {e.isDir ? <Folder className="entry-icon" /> : <FileIcon className="entry-icon" />}
                      <span>{e.name}</span>
                    </td>
                    <td className="col-size">{e.isDir ? "—" : fmtSize(e.size)}</td>
                    <td className="col-mtime">{shortDate(e.mtime)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>

        <section className="preview">
          <div className="tabbar">
            {tabs.map((t) => (
              <div
                key={t.id}
                className={`tab ${t.id === activeTabId ? "active" : ""}`}
                onClick={() => setActiveTabId(t.id)}
                title={t.path}
              >
                <FileText className="tab-icon" />
                <span className="tab-title">{t.name}</span>
                <button
                  className="tab-close"
                  onClick={(ev) => {
                    ev.stopPropagation();
                    closeTab(t.id);
                  }}
                  aria-label="关闭标签"
                >
                  ×
                </button>
              </div>
            ))}
            {tabs.length === 0 && (
              <div className="tabbar-hint">双击文件预览；拖入文件亦可</div>
            )}
            <div className="tabbar-spacer" />
          </div>
          <div className="preview-body">
            {!active ? (
              <div className="empty">
                <div className="empty-icon">📂</div>
                <p>从左侧资源管理器浏览；双击文件预览</p>
                <p className="empty-sub">
                  支持 Markdown / 文本 / 代码 / 图片 / PDF / Office / HTML
                </p>
              </div>
            ) : (
              <PreviewHost
                key={active.id}
                path={active.path}
                name={active.name}
                source={viewerFileSource}
                isDark={false}
              />
            )}
          </div>
        </section>

        {aiOpen && (
          <AiSidebar
            filePath={active?.path ?? null}
            selection={selection}
            onClose={() => setAiOpen(false)}
          />
        )}
      </div>

      {ctxMenu && (
        <div className="ctxmenu" style={{ left: ctxMenu.x, top: ctxMenu.y }}>
          <button
            className="ctxmenu-item"
            onClick={() => {
              if (ctxMenu.entry.isDir) navigate(ctxMenu.entry.path, true);
              else openPath(ctxMenu.entry.path);
              setCtxMenu(null);
            }}
          >
            <FolderOpen /> {ctxMenu.entry.isDir ? "进入文件夹" : "打开"}
          </button>
          <button
            className="ctxmenu-item"
            onClick={() => {
              copyPath(ctxMenu.entry.path);
              setCtxMenu(null);
            }}
          >
            <Copy /> 复制完整路径
          </button>
          <button
            className="ctxmenu-item"
            onClick={async () => {
              await viewerFileSource.openWithSystem?.(ctxMenu.entry.path);
              setCtxMenu(null);
            }}
            disabled={!viewerFileSource.openWithSystem}
          >
            <ExternalLink /> 用系统程序打开
          </button>
          <button
            className="ctxmenu-item"
            onClick={async () => {
              await viewerFileSource.revealInExplorer?.(ctxMenu.entry.path);
              setCtxMenu(null);
            }}
            disabled={!viewerFileSource.revealInExplorer}
          >
            <Eye /> 在资源管理器中显示
          </button>
        </div>
      )}

      {dragOver && <div className="drop-overlay">松开以打开文件</div>}
    </div>
  );
}
