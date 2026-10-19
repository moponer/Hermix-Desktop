// Hermix 查看器主界面：单 WebView 内 资源管理器 + 多 tab 预览。
//
// 决策笔记 2026-10-13：viewer 一期是「拖入文件 + 多 tab」，二期是
// 「真实资源管理器」。这一版把两侧合并：左侧树 + 中部列表 + 右侧预览，
// 文件双击进入预览 tab（PDF/图片/Office …），新 tab 不会持续再开窗口。
//
// 共享包走 @hermix/preview —— 与桌面 app 用同一份解析器、同一份样式层。

import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  ArrowLeft, ArrowRight, ArrowUp, RefreshCw, Search, Eye, EyeOff,
  ChevronRight, FolderOpen, HardDrive, Copy, ExternalLink, Sparkles, X,
  Monitor, Moon, Star, Sun,
} from "lucide-react";
import { FileTypeIcon, FolderTypeIcon } from "@hermix/preview/FileTypeIcon";
import type { DirEntryLike, DriveRootLike } from "@hermix/preview";
import { viewerFileSource } from "./fileSource";
import { useTheme, type ThemeMode } from "./theme";
import { AiSidebar } from "./AiSidebar";

// 预览引擎懒加载：PreviewHost → @hermix/preview barrel → satteri 的 3MB
// WASM + ExcelJS + pdf.js + pptx。静态引入会让整条预览栈进首屏 bundle，
// webview 首次加载被拖到 10s+，Rust 侧主线程等它加载完才处理 IPC →
// 文件列表 20s 后才出现（实测 21.48s）。改成懒加载后，"浏览文件"不再
// 等"预览引擎"，与主程序 main.tsx 的分窗小图是同款思路。
const PreviewHost = lazy(() => import("./PreviewHost").then((m) => ({ default: m.PreviewHost })));

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

interface Bookmark {
  path: string;
  name: string;
  isDir: boolean;
}

/** 目录树节点状态：children 为 null 表示尚未加载（懒加载）。 */
interface TreeNodeState {
  expanded: boolean;
  children: DirEntryLike[] | null;
}

const BOOKMARKS_KEY = "hermix.viewer.bookmarks";
/** 上次访问目录：首屏兜底路径的第一优先级（不依赖驱动器枚举）。 */
const LAST_PATH_KEY = "hermix.viewer.lastPath";
/** 驱动器枚举超过这个时长还没回来 → 侧栏转「慢」态，不再显示骨架。 */
const DRIVES_TIMEOUT_MS = 1500;
/** 栏宽持久化键。 */
const LAYOUT_KEY = "hermix.viewer.layout";

const THEME_LABEL: Record<ThemeMode, string> = {
  system: "跟随系统",
  light: "浅色",
  dark: "深色",
};

let nextTabId = 1;

function readBookmarks(): Bookmark[] {
  try {
    const arr = JSON.parse(localStorage.getItem(BOOKMARKS_KEY) ?? "[]");
    if (!Array.isArray(arr)) return [];
    return arr.filter(
      (b): b is Bookmark =>
        b != null &&
        typeof b.path === "string" &&
        typeof b.name === "string" &&
        typeof b.isDir === "boolean",
    );
  } catch {
    return [];
  }
}

function driveToEntry(d: DriveRootLike): DirEntryLike {
  return { name: d.label, path: d.path, isDir: true, size: 0, mtime: null, extension: "", hidden: false };
}

/** 上次访问目录：首屏兜底路径的第一优先级（不依赖驱动器枚举）。 */
function readLastPath(): string | null {
  try {
    const p = localStorage.getItem(LAST_PATH_KEY);
    if (!p) return null;
    // 只认盘符开头或 UNC 开头：相对路径会随工作目录漂移到别处。
    if (!/^([a-zA-Z]:|\\\\)/.test(p)) return null;
    return p;
  } catch {
    return null;
  }
}

/**
 * 栏宽布局：侧栏固定 px、文件列表按**比例**、AI 栏固定 px。
 *
 * 文件列表用比例而不是绝对像素——窗口拉大 / 全屏时它跟着铺满，
 * 这正是"全屏下组件没有平铺"的病根：原先 `.col-name` 被钉死在
 * `max-width: 240px`、`.explorer` 被钉死在 `max-width: 460px`，
 * 宽屏下表格右侧全是空白。比例列在 1080p 与 4K 上都按同一份"观感"分栏。
 */
interface Layout {
  sbW: number;
  exRatio: number;
  aiW: number;
}

const LAYOUT_DEFAULT: Layout = { sbW: 200, exRatio: 0.3, aiW: 320 };

function clamp(v: number, lo: number, hi: number): number {
  return Math.min(hi, Math.max(lo, v));
}

function readLayout(): Layout {
  try {
    const raw = JSON.parse(localStorage.getItem(LAYOUT_KEY) ?? "");
    if (!raw || typeof raw !== "object") return LAYOUT_DEFAULT;
    const l = raw as Partial<Layout>;
    return {
      sbW: clamp(typeof l.sbW === "number" ? l.sbW : LAYOUT_DEFAULT.sbW, 140, 420),
      exRatio: clamp(
        typeof l.exRatio === "number" ? l.exRatio : LAYOUT_DEFAULT.exRatio,
        0.16,
        0.6,
      ),
      aiW: clamp(typeof l.aiW === "number" ? l.aiW : LAYOUT_DEFAULT.aiW, 240, 560),
    };
  } catch {
    return LAYOUT_DEFAULT;
  }
}

/** 栏宽拖拽条：拖动改相邻栏宽度，方向键也能调（无障碍）。 */
function Resizer({
  onDelta,
  label,
}: {
  onDelta: (delta: number) => void;
  label: string;
}) {
  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      e.preventDefault();
      let lastX = e.clientX;
      const move = (ev: PointerEvent) => {
        const dx = ev.clientX - lastX;
        lastX = ev.clientX;
        onDelta(dx);
      };
      const up = () => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        document.body.style.userSelect = "";
      };
      document.body.style.userSelect = "none";
      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
    },
    [onDelta],
  );
  return (
    <div
      className="resizer"
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onKeyDown={(e) => {
        if (e.key === "ArrowLeft") {
          e.preventDefault();
          onDelta(-16);
        } else if (e.key === "ArrowRight") {
          e.preventDefault();
          onDelta(16);
        }
      }}
    />
  );
}

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

/** 目录树行：递归渲染，子节点懒加载（children 为 null 时点开才取）。 */
function TreeRow({
  entry, depth, tree, currentPath, onToggle, onOpen,
}: {
  entry: DirEntryLike;
  depth: number;
  tree: Record<string, TreeNodeState>;
  currentPath: string | null;
  onToggle: (path: string) => void;
  onOpen: (entry: DirEntryLike) => void;
}) {
  const st = tree[entry.path];
  const expanded = st?.expanded ?? false;
  const children = st?.children ?? null;
  return (
    <>
      <div
        className={`sb-row ${currentPath === entry.path ? "is-active" : ""}`}
        style={{ paddingLeft: depth * 12 }}
      >
        {entry.isDir && (
          <button
            type="button"
            className="sb-row-caret"
            onClick={() => onToggle(entry.path)}
            aria-expanded={expanded}
            aria-label={expanded ? `收起 ${entry.name}` : `展开 ${entry.name}`}
          >
            <ChevronRight className={expanded ? "is-open" : ""} />
          </button>
        )}
        <button type="button" className="sb-row-label" onClick={() => onOpen(entry)} title={entry.path}>
          {depth === 0 ? (
            <HardDrive className="entry-icon" />
          ) : (
            <FolderTypeIcon size={16} open={expanded} className="entry-icon" />
          )}
          <span>{entry.name}</span>
        </button>
      </div>
      {expanded && children?.length === 0 && (
        <div className="sb-row-empty" style={{ paddingLeft: depth * 12 + 22 }}>空目录</div>
      )}
      {expanded &&
        children?.map((c) => (
          <TreeRow
            key={c.path}
            entry={c}
            depth={depth + 1}
            tree={tree}
            currentPath={currentPath}
            onToggle={onToggle}
            onOpen={onOpen}
          />
        ))}
    </>
  );
}

export function App() {
  // —— 资源管理器状态 ——
  const [currentPath, setCurrentPath] = useState<string | null>(null);
  const [pathInput, setPathInput] = useState("");
  const [entries, setEntries] = useState<DirEntryLike[]>([]);
  const [drivesState, setDrivesState] = useState<{
    status: "loading" | "slow" | "ready";
    items: DriveRootLike[];
  }>({ status: "loading", items: [] });
  const drives = drivesState.items;

  // —— 栏宽布局（可拖拽；文件列表按比例，窗口变大自动铺满） ——
  const [layout, setLayout] = useState<Layout>(readLayout);
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

  // —— 书签（localStorage 持久化） ——
  const [bookmarks, setBookmarks] = useState<Bookmark[]>(readBookmarks);

  // —— 目录树（懒加载，键为目录路径） ——
  const [tree, setTree] = useState<Record<string, TreeNodeState>>({});

  // —— 主题 ——
  const { resolved: theme, mode: themeMode, cycle: cycleTheme } = useTheme();

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
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 驱动器列表：只请求一次，与首屏解耦。Rust 侧已不碰文件系统，但网络盘
  // 枚举仍可能慢——超过 DRIVES_TIMEOUT_MS 就转「慢」态（骨架换成提示），
  // 后台继续等结果，拿到就覆盖。
  useEffect(() => {
    if (!viewerFileSource.fs) return;
    let settled = false;
    const timer = setTimeout(() => {
      if (!settled) {
        setDrivesState((s) => (s.status === "loading" ? { ...s, status: "slow" } : s));
      }
    }, DRIVES_TIMEOUT_MS);
    void viewerFileSource.fs.drives().then((ds) => {
      settled = true;
      clearTimeout(timer);
      setDrivesState({ status: "ready", items: ds });
    });
    return () => clearTimeout(timer);
  }, []);

  // 首屏目录：不等驱动器。兜底顺序 = 上次访问目录 → 用户主目录 →
  // 首个盘符（最后一项要等驱动器到位，只在前两项都拿不到时才走）。
  useEffect(() => {
    if (currentPath !== null) return;
    let cancelled = false;
    void (async () => {
      const last = readLastPath();
      if (last) {
        if (!cancelled) setCurrentPath(last);
        return;
      }
      const home = await invoke<string | null>("viewer_home_dir").catch(() => null);
      if (cancelled) return;
      if (home) {
        setCurrentPath(home);
        return;
      }
      const ds = viewerFileSource.fs ? await viewerFileSource.fs.drives() : [];
      if (!cancelled && ds.length > 0) setCurrentPath(ds[0].path);
    })();
    return () => {
      cancelled = true;
    };
  }, [currentPath]);

  // 当前目录持久化：下次启动直接回到这里，路径栏同步显示。
  useEffect(() => {
    if (!currentPath) return;
    setPathInput(currentPath);
    try {
      localStorage.setItem(LAST_PATH_KEY, currentPath);
    } catch {
      /* 持久化失败仅影响下次启动，本次会话内仍可用 */
    }
  }, [currentPath]);

  // 栏宽持久化。
  useEffect(() => {
    try {
      localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
    } catch {
      /* 持久化失败仅影响下次启动，本次会话内仍可用 */
    }
  }, [layout]);

  const onSbDelta = useCallback((dx: number) => {
    setLayout((l) => ({ ...l, sbW: clamp(l.sbW + dx, 140, 420) }));
  }, []);
  const onExDelta = useCallback((dx: number) => {
    setLayout((l) => ({
      ...l,
      // 比例列按窗口宽度换算成比例增量：拖动 100px ≈ 窗口的 8%。
      exRatio: clamp(l.exRatio + dx / Math.max(window.innerWidth, 800), 0.16, 0.6),
    }));
  }, []);
  const onAiDelta = useCallback((dx: number) => {
    setLayout((l) => ({ ...l, aiW: clamp(l.aiW - dx, 240, 560) }));
  }, []);

  const layoutVars = useMemo(
    () =>
      ({
        "--sb-w": `${layout.sbW}px`,
        "--ex-ratio": layout.exRatio,
        "--ai-w": `${layout.aiW}px`,
      }) as unknown as CSSProperties,
    [layout],
  );

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

  // — 书签 —
  useEffect(() => {
    try {
      localStorage.setItem(BOOKMARKS_KEY, JSON.stringify(bookmarks));
    } catch {
      /* 持久化失败仅影响下次启动，本次会话内仍可用 */
    }
  }, [bookmarks]);

  const toggleBookmark = useCallback((entry: DirEntryLike) => {
    setBookmarks((bs) =>
      bs.some((b) => b.path === entry.path)
        ? bs.filter((b) => b.path !== entry.path)
        : [...bs, { path: entry.path, name: entry.name, isDir: entry.isDir }],
    );
  }, []);

  const openBookmark = useCallback(
    (b: Bookmark) => {
      if (b.isDir) {
        navigate(b.path, true);
        return;
      }
      // 文件书签：列表跳到父目录 + 直接开预览 tab
      void viewerFileSource.fs?.parent(b.path).then((p) => {
        if (p) navigate(p, true);
      });
      openPath(b.path);
    },
    [navigate, openPath],
  );

  // — 目录树 —
  const loadTreeChildren = useCallback(
    async (path: string) => {
      if (!viewerFileSource.fs) return;
      const r = await viewerFileSource.fs.listDir(path, showHidden);
      setTree((t) => ({
        ...t,
        [path]: { expanded: true, children: r.ok ? r.entries.filter((e) => e.isDir) : [] },
      }));
    },
    [showHidden],
  );

  const toggleTreeNode = useCallback(
    (path: string) => {
      const st = tree[path];
      if (st?.expanded) {
        setTree((t) => ({ ...t, [path]: { ...t[path], expanded: false } }));
      } else if (st?.children) {
        setTree((t) => ({ ...t, [path]: { ...t[path], expanded: true } }));
      } else {
        void loadTreeChildren(path);
      }
    },
    [tree, loadTreeChildren],
  );

  const onTreeOpen = useCallback(
    (entry: DirEntryLike) => {
      navigate(entry.path, true);
      if (tree[entry.path]?.children) {
        setTree((t) => ({ ...t, [entry.path]: { ...t[entry.path], expanded: true } }));
      } else {
        void loadTreeChildren(entry.path);
      }
    },
    [navigate, tree, loadTreeChildren],
  );

  // 隐藏文件开关变化：已展开节点的子列表按新可见性重取（展开状态保留）
  const treeRef = useRef(tree);
  treeRef.current = tree;
  useEffect(() => {
    for (const [p, v] of Object.entries(treeRef.current)) {
      if (v.expanded) void loadTreeChildren(p);
    }
  }, [showHidden, loadTreeChildren]);

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
      style={layoutVars}
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
        <button className="tb-btn" onClick={cycleTheme} title={`主题：${THEME_LABEL[themeMode]}`}>
          {themeMode === "system" ? <Monitor /> : themeMode === "light" ? <Sun /> : <Moon />}
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
          {bookmarks.length > 0 && (
            <>
              <div className="sb-head">书签</div>
              {bookmarks.map((b) => (
                <div
                  key={b.path}
                  className={`sb-row ${currentPath === b.path ? "is-active" : ""}`}
                >
                  <button
                    type="button"
                    className="sb-row-label"
                    onClick={() => openBookmark(b)}
                    title={b.path}
                  >
                    {b.isDir ? (
                      <FolderTypeIcon size={16} className="entry-icon" />
                    ) : (
                      <FileTypeIcon name={b.name} size={16} className="entry-icon" />
                    )}
                    <span>{b.name}</span>
                  </button>
                  <button
                    type="button"
                    className="sb-row-aux"
                    onClick={() => setBookmarks((bs) => bs.filter((x) => x.path !== b.path))}
                    aria-label={`移除书签 ${b.name}`}
                  >
                    ×
                  </button>
                </div>
              ))}
            </>
          )}
          <div className="sb-head">本机</div>
          {drivesState.status === "loading" ? (
            // 骨架屏：驱动器还没回来，但位置已经占好——不留空白侧栏。
            <div className="sb-skeleton">
              <div className="sb-skeleton-row" />
              <div className="sb-skeleton-row" />
              <div className="sb-skeleton-row" />
            </div>
          ) : (
            drives.map((d) => (
              <TreeRow
                key={d.path}
                entry={driveToEntry(d)}
                depth={0}
                tree={tree}
                currentPath={currentPath}
                onToggle={toggleTreeNode}
                onOpen={onTreeOpen}
              />
            ))
          )}
          {drivesState.status === "slow" && (
            <div className="sb-hint">部分驱动器无响应，正在继续读取…</div>
          )}
        </aside>

        <Resizer onDelta={onSbDelta} label="调整侧栏宽度" />

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
                      {/* 图标 + 名字包一层 flex 容器：td 保持 table-cell
                          身份，列宽才能由 table-layout: fixed 分配，
                          名字列吃掉剩余宽度（宽屏下不再留空白）。 */}
                      <span className="cell-main">
                        {e.isDir ? (
                          <FolderTypeIcon size={16} className="entry-icon" />
                        ) : (
                          <FileTypeIcon name={e.name} size={16} className="entry-icon" />
                        )}
                        <span className="cell-text">{e.name}</span>
                      </span>
                    </td>
                    <td className="col-size">{e.isDir ? "—" : fmtSize(e.size)}</td>
                    <td className="col-mtime">{shortDate(e.mtime)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>

        <Resizer onDelta={onExDelta} label="调整文件列表宽度" />

        <section className="preview">
          <div className="tabbar">
            {tabs.map((t) => (
              <div
                key={t.id}
                className={`tab ${t.id === activeTabId ? "active" : ""}`}
                onClick={() => setActiveTabId(t.id)}
                title={t.path}
              >
                <FileTypeIcon name={t.name} size={14} className="tab-icon" />
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
              <Suspense fallback={<div className="preview-pane" />}>
                <PreviewHost
                  key={active.id}
                  path={active.path}
                  name={active.name}
                  source={viewerFileSource}
                  isDark={theme === "dark"}
                />
              </Suspense>
            )}
          </div>
        </section>

        {aiOpen && (
          <>
            <Resizer onDelta={onAiDelta} label="调整 AI 栏宽度" />
            <AiSidebar
              // key 绑当前文件：切换标签时重建面板 → 会话 id 随之重置。
              // 否则不关 AI 栏换文件，新文件路径会被拼进上一个文件的会话里。
              key={active?.path ?? "__none__"}
              filePath={active?.path ?? null}
              selection={selection}
              onClose={() => setAiOpen(false)}
            />
          </>
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
              toggleBookmark(ctxMenu.entry);
              setCtxMenu(null);
            }}
          >
            <Star /> {bookmarks.some((b) => b.path === ctxMenu.entry.path) ? "移除书签" : "添加书签"}
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
