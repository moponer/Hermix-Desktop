// Hermix 文件查看器主界面：单 WebView 内 资源管理器 + 多 tab 预览。
//
// 决策笔记 2026-10-13：viewer 一期是「拖入文件 + 多 tab」，二期是
// 「真实资源管理器」。这一版把两侧合并：左侧树 + 中部列表 + 右侧预览，
// 单击文件进入预览 tab（PDF/图片/Office …），新 tab 不会持续再开窗口。
//
// 共享包走 @hermix/preview —— 与桌面 app 用同一份解析器、同一份样式层。

import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  ArrowLeft, ArrowRight, ArrowUp, RefreshCw, Search,
  Funnel, FunnelX, FileSearch,
  ChevronRight, HardDrive, ExternalLink, Sparkles, X,
  Monitor, Moon, Sun, Activity, Info, LogOut, Settings2, Pencil,
} from "lucide-react";
import { FileTypeIcon, FolderTypeIcon } from "@hermix/preview/FileTypeIcon";
import { resolveKind } from "@hermix/preview";
import type { DirEntryLike, DriveRootLike } from "@hermix/preview";
import { viewerFileSource } from "./fileSource";
import { useTheme, type ThemeMode } from "./theme";
import { AiSidebar, type PendingSend } from "./AiSidebar";
import { joinConversationPrompt } from "./aiPrompts";
import { ContextMenu } from "./ContextMenu";
import { TitleBar } from "./TitleBar";
import { AboutDialog, SystemPanel } from "./SystemPanel";
import type { MenuGroup } from "./MenuBar";
import { buildMenu, type MenuTarget, type SortKind } from "./contextMenuModel";
import { SettingsDialog } from "./SettingsDialog";
import { DEFAULT_SETTINGS, loadSettings, type ViewerSettings } from "./settings";
import { editorFor, editorLabel } from "./editKind";

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
/** 列表排序方式持久化键（右键菜单可切）。 */
const SORT_KEY = "hermix.viewer.sort";
/** 视图模式：纯净（只预览）/ 重量（资源管理器 + 预览 + AI + 工具）。 */
type ViewerMode = "pure" | "heavy";
const MODE_KEY = "hermix.viewer.mode";

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

/** 上次显式选择的视图模式；从未选过返回 null（此时由启动场景决定）。 */
function readMode(): ViewerMode | null {
  try {
    const v = localStorage.getItem(MODE_KEY);
    return v === "pure" || v === "heavy" ? v : null;
  } catch {
    return null;
  }
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

/**
 * 从 DOM 元素的 data-ctx 系列属性还原右键落点。
 *
 * 组件只负责标属性（data-ctx / data-ctx-path / data-ctx-name /
 * data-ctx-dir / data-ctx-from），菜单内容由 contextMenuModel 按 kind
 * 决定——新增一类落点只要加一个 kind，不用改所有组件。
 */
function resolveCtxTarget(el: HTMLElement | null): MenuTarget {
  if (!el) return { kind: "app" };
  const path = el.dataset.ctxPath ?? "";
  const name = el.dataset.ctxName ?? "";
  switch (el.dataset.ctx) {
    case "entry":
      return {
        kind: "entry",
        path,
        name,
        isDir: el.dataset.ctxDir === "1",
        from: el.dataset.ctxFrom === "tree" || el.dataset.ctxFrom === "bookmark"
          ? el.dataset.ctxFrom
          : "list",
      };
    case "explorer":
      return { kind: "explorer" };
    case "tab":
      return { kind: "tab", id: Number(el.dataset.ctxId ?? "0"), path, name };
    case "preview":
      return { kind: "preview", path, name };
    case "pathbar":
      return { kind: "pathbar" };
    default:
      break;
  }
  // 其它输入框（搜索框、AI 输入框）：给文本编辑菜单——
  // 拦掉原生右键之后，没有菜单的话右键就没法粘贴了。
  if (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable) {
    return { kind: "field" };
  }
  return { kind: "app" };
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
        data-ctx="entry"
        data-ctx-path={entry.path}
        data-ctx-name={entry.name}
        data-ctx-dir={entry.isDir ? "1" : "0"}
        data-ctx-from="tree"
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

  // —— 视图模式：纯净（只预览）/ 重量（资源管理器 + 预览 + AI + 工具） ——
  const [mode, setMode] = useState<ViewerMode>("heavy");
  const [showHidden, setShowHidden] = useState(false);
  const [searchQ, setSearchQ] = useState("");
  const [searching, setSearching] = useState(false);
  const [searchResults, setSearchResults] = useState<DirEntryLike[]>([]);
  /**
   * 搜索结果的来源：global = 全盘索引（类 Everything），local = 当前目录。
   * 用户要能一眼看出「搜的是全盘还是这一层」——两者结果范围差几个数量级。
   */
  const [searchScope, setSearchScope] = useState<"global" | "local" | null>(null);
  /** 搜索来源的补充说明（索引不可用时说明为什么退回目录搜索）。 */
  const [searchNote, setSearchNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  /** 列表排序方式：name（后端默认）/ size / mtime。 */
  const [sortBy, setSortBy] = useState<SortKind>(() => {
    try {
      const v = localStorage.getItem(SORT_KEY);
      return v === "size" || v === "mtime" || v === "name" ? v : "name";
    } catch {
      return "name";
    }
  });

  // —— 多 tab + 预览 ——
  const [tabs, setTabs] = useState<Tab[]>([]);
  const [activeTabId, setActiveTabId] = useState<number | null>(null);
  /** 当前激活的标签（预览区、标题栏、菜单都要用）。 */
  const active = tabs.find((t) => t.id === activeTabId) ?? null;

  // —— AI 右栏 ——
  const [aiOpen, setAiOpen] = useState(false);
  const [aiPending, setAiPending] = useState<PendingSend | null>(null);
  const [selection, setSelection] = useState("");

  // —— 标题栏菜单弹出的窗口 ——
  const [sysOpen, setSysOpen] = useState(false);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [settingsOpen, setSettingsOpen] = useState(false);
  /** 设置（搜索行为 / AI 后端 / MCP / 通用偏好）。Rust 侧是权威源。 */
  const [settings, setSettings] = useState<ViewerSettings>(DEFAULT_SETTINGS);
  /** 打开编辑器的请求号（右键 / 菜单 / 快捷键共用同一个入口）。 */
  const [editRequest, setEditRequest] = useState(0);

  // —— 导航历史 ——
  const [backStack, setBackStack] = useState<string[]>([]);
  const [forwardStack, setForwardStack] = useState<string[]>([]);

  // —— 右键菜单（全局拦截原生右键，落点由 data-ctx 属性决定） ——
  const [menu, setMenu] = useState<{
    x: number;
    y: number;
    target: MenuTarget;
    /** 键盘唤起（detail === 0）——键盘用户要能直接用方向键走菜单。 */
    viaKeyboard: boolean;
    /** 输入框里选中的文字（没有选区时为空——菜单据此把「复制选区」置灰）。 */
    fieldSelection: string;
  } | null>(null);
  /** 本机可用的「打开方式」（notepad / code…）——没装的在菜单里置灰。 */
  const [openApps, setOpenApps] = useState<string[]>([]);
  /** 一次性轻提示（复制/粘贴/打开的结果反馈）。 */
  const [notice, setNotice] = useState<{ text: string; kind: "ok" | "err" } | null>(null);
  const noticeTimer = useRef<number | undefined>(undefined);
  /** 菜单打开时聚焦着的输入框（文本编辑动作作用在它身上）。 */
  const fieldRef = useRef<HTMLInputElement | HTMLTextAreaElement | null>(null);

  // —— 书签（localStorage 持久化） ——
  const [bookmarks, setBookmarks] = useState<Bookmark[]>(readBookmarks);

  // —— 目录树（懒加载，键为目录路径） ——
  const [tree, setTree] = useState<Record<string, TreeNodeState>>({});

  // —— 主题 ——
  const { resolved: theme, mode: themeMode, cycle: cycleTheme, set: setTheme } = useTheme();

  // —— 拖放 ——
  const [dragOver, setDragOver] = useState(false);
  const openedRef = useRef<Set<string>>(new Set());
  /** 待补做的「跟随定位」目录：驱动器列表还没回来时先记着（首屏并发）。 */
  const pendingSyncRef = useRef<string | null>(null);
  /** 「加入 AI 对话」的请求序号：每次自增，保证同一条消息不被发两遍。 */
  const aiNonceRef = useRef(0);

  // 启动参数 / 单实例投递（与旧实现一致）
  useEffect(() => {
    void invoke<string[]>("viewer_take_pending").then((paths) => {
      // 启动场景决定初始模式：
      //   - 无路径启动 = 主程序的「文件管理器」入口 → 重量模式（资源管理器）。
      //     这里**不看**上次显式选择：那个动作的意思是「打开文件管理器」，
      //     上次预览文件时用的是纯净模式，不该把入口变成一个空预览页。
      //   - 带路径启动：被文件关联唤起 → 纯净模式（"双击就看"，不带侧栏与 AI）；
      //     用户显式切过的模式优先——显式选择永远压过默认推断。
      setMode(paths.length === 0 ? "heavy" : (readMode() ?? "pure"));
      const load = async () => {
        for (const p of paths) await openTarget(p);
      };
      void load();
    });
    void import("@tauri-apps/api/event").then(async ({ listen }) => {
      // 单实例投递：目录进资源管理器，文件开预览（与启动参数同一条判别）。
      await listen<string>("viewer-open-file", (e) => {
        void openTarget(e.payload);
      });
      // 「打开文件管理器」投递（主程序入口，无路径）：没有文件可开——切到
      // 资源管理器模式（窗口已由 Rust 侧叫到前面）。
      await listen("viewer-open-explorer", () => {
        switchMode("heavy");
      });
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 设置：Rust 侧是权威源（$hermix_home/viewer-settings.json）。
  // 必须在 effect 里取一次：写在组件体里会**每次渲染都发一遍 IPC**，而且
  // setSettings 每次生成新对象 → runSearch 每次重建 → 搜索的去抖定时器
  // 每次被清掉，实时搜索永远触发不了。
  useEffect(() => {
    void loadSettings().then((s) => {
      setSettings(s);
      // 「启动时显示隐藏文件」是一次性偏好：落到当前会话状态上。
      if (s.general.showHiddenByDefault) setShowHidden(true);
    });
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

  /** 切换视图模式并持久化（显式选择压过启动场景推断）。 */
  const switchMode = useCallback((m: ViewerMode) => {
    setMode(m);
    try {
      localStorage.setItem(MODE_KEY, m);
    } catch {
      /* 持久化失败仅影响下次启动，本次会话内仍可用 */
    }
  }, []);

  const clearAiPending = useCallback(() => setAiPending(null), []);

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
      // 换目录 = 搜索作废：范围标签必须一起清，否则用户会以为看到的是
      // 刚才那次搜索的结果。
      setSearchScope(null);
      setSearchNote(null);
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

  // ── 标题栏菜单 ─────────────────────────────────────────────────────
  // 菜单是"命令表 + 分发"，不放业务逻辑：每一项只映射到一个已有动作。

  /**
   * 当前文件能不能编辑 + 「编辑」这一项该怎么写。
   *
   * 判定只有一处（editKind.ts）：重量模式 + 该类型有编辑器。不能编辑时
   * 菜单项置灰（而不是不显示）——用户看到「编辑」置灰才知道这个格式改不了。
   */
  const activeEditorKind = useMemo(() => {
    if (mode !== "heavy" || !active) return "none";
    return editorFor(resolveKind(active.name), active.name);
  }, [active, mode]);
  const requestEdit = useCallback(() => setEditRequest((n) => n + 1), []);

  const menus = useMemo<MenuGroup[]>(
    () => [
      {
        id: "file",
        label: "文件",
        items: [
          { id: "refresh", label: "刷新", shortcut: "F5", icon: <RefreshCw size={13} /> },
          { id: "close-tabs", label: "关闭所有标签", icon: <X size={13} /> },
          "-",
          { id: "quit", label: "退出", danger: true, icon: <LogOut size={13} /> },
        ],
      },
      {
        id: "view",
        label: "视图",
        items: [
          {
            id: "toggle-hidden",
            label: "显示隐藏文件",
            checked: showHidden,
            icon: <Funnel size={13} />,
          },
          { id: "toggle-ai", label: "AI 助手栏", checked: aiOpen, icon: <Sparkles size={13} /> },
          "-",
          {
            id: "theme-system",
            label: "主题：跟随系统",
            checked: themeMode === "system",
            icon: <Monitor size={13} />,
          },
          {
            id: "theme-light",
            label: "主题：浅色",
            checked: themeMode === "light",
            icon: <Sun size={13} />,
          },
          {
            id: "theme-dark",
            label: "主题：深色",
            checked: themeMode === "dark",
            icon: <Moon size={13} />,
          },
        ],
      },
      {
        id: "tools",
        label: "工具",
        items: [
          {
            id: "edit",
            label: editorLabel(activeEditorKind),
            shortcut: "Ctrl+E",
            disabled: activeEditorKind === "none",
            icon: <Pencil size={13} />,
          },
          {
            id: "sys-panel",
            label: "系统资源管理…",
            shortcut: "Ctrl+Shift+Esc",
            icon: <Activity size={13} />,
          },
          "-",
          { id: "settings", label: "设置…", icon: <Settings2 size={13} /> },
        ],
      },
      {
        id: "help",
        label: "帮助",
        items: [{ id: "about", label: "关于 Hermix 文件查看器", icon: <Info size={13} /> }],
      },
    ],
    [showHidden, aiOpen, themeMode, activeEditorKind],
  );

  const onMenuCommand = useCallback(
    (id: string) => {
      switch (id) {
        case "refresh":
          refresh();
          break;
        case "close-tabs":
          setTabs([]);
          setActiveTabId(null);
          break;
        case "quit":
          void getCurrentWindow().close();
          break;
        case "toggle-hidden":
          setShowHidden((v) => !v);
          break;
        case "toggle-ai":
          setAiOpen((v) => !v);
          break;
        case "theme-system":
          setTheme("system");
          break;
        case "theme-light":
          setTheme("light");
          break;
        case "theme-dark":
          setTheme("dark");
          break;
        case "sys-panel":
          setSysOpen(true);
          break;
        case "edit":
          requestEdit();
          break;
        case "settings":
          setSettingsOpen(true);
          break;
        case "about":
          setAboutOpen(true);
          break;
        default:
          break;
      }
    },
    [refresh, setTheme],
  );

  // 快捷键：F5 刷新、Ctrl+Shift+Esc 打开系统资源管理（与任务管理器同键位）。
  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "F5") {
        event.preventDefault();
        refresh();
        return;
      }
      if (event.ctrlKey && event.shiftKey && event.key === "Escape") {
        event.preventDefault();
        setSysOpen(true);
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [refresh]);

  const exitSearch = useCallback(() => {
    setSearchQ("");
    setSearchResults([]);
    setSearching(false);
    setSearchScope(null);
    setSearchNote(null);
  }, []);

  /**
   * 搜索：默认先走全盘索引（类 Everything），索引服务不可用时退回当前目录。
   *
   * 为什么不只做目录搜索：目录搜索是「在这一层里翻一遍」，用户要找的
   * 文件多半不在当前层——那等于没有搜索。全盘索引跨卷、毫秒级返回，
   * 是 Everything 的体验。但索引服务可能没跑或没授权，这时**必须**退回
   * 目录搜索：一次索引失败不该让整条搜索链路断掉。
   *
   * 范围/条数/隐藏文件由设置决定（设置 → 搜索）：scope=local 时干脆不碰
   * 全盘索引——用户明确选了「只搜当前目录」，再去请求一次全盘是白等。
   */
  const runSearch = useCallback(
    async (query: string) => {
      const q = query.trim();
      if (!q) {
        exitSearch();
        return;
      }
      const limit = settings.search.limit;
      const scope = settings.search.scope;
      setSearchQ(q);
      setSearching(true);
      // 1. 全盘索引
      if (scope !== "local") {
        setSearchNote("正在搜索全盘索引（首次会请求管理员授权以建立索引）…");
        const global = await viewerFileSource.searchGlobal?.(q, limit);
        if (global?.ok) {
          setSearchResults(global.entries);
          setSearchScope("global");
          setSearchNote(
            global.totalMatches > global.entries.length
              ? `全盘索引命中 ${global.totalMatches} 条，显示前 ${global.entries.length} 条`
              : `全盘索引命中 ${global.totalMatches} 条`,
          );
          setSearching(false);
          return;
        }
        if (scope === "global") {
          // 用户选了「只搜全盘」：索引不可用就如实说，不偷偷换范围。
          setSearchResults([]);
          setSearchScope("global");
          setSearchNote(global?.message ?? "全盘索引不可用");
          setSearching(false);
          return;
        }
      }
      const reason = "全盘索引不可用";
      // 2. 当前目录
      if (!currentPath || !viewerFileSource.fs) {
        setSearchResults([]);
        setSearchScope("local");
        setSearchNote(`${reason}；当前没有目录可搜`);
        setSearching(false);
        return;
      }
      const local = await viewerFileSource.fs.search(
        currentPath,
        q,
        settings.search.includeHidden || showHidden,
        limit,
      );
      if (local.ok) {
        setSearchResults(local.entries);
        setSearchScope("local");
        setSearchNote(
          scope === "local" ? "仅在当前目录内搜索" : `仅在当前目录内搜索（${reason}）`,
        );
      } else {
        setSearchResults([]);
        setSearchScope("local");
        setSearchNote(`搜索失败：${local.message}`);
      }
      setSearching(false);
    },
    [currentPath, exitSearch, showHidden, settings.search, viewerFileSource],
  );

  /**
   * 输入即搜（live 模式）：去抖后自动搜，回车仍然立刻搜。
   *
   * 去抖是必须的：全盘索引虽然毫秒级，但每个键都打一次索引 + 一次
   * Rust IPC 往返，手速稍快就会把索引服务打爆。条数/范围/隐藏文件
   * 由设置决定（设置 → 搜索）。
   */
  const searchTimerRef = useRef<number | undefined>(undefined);
  useEffect(() => {
    if (settings.search.mode !== "live") return undefined;
    const q = searchQ.trim();
    if (!q) return undefined;
    if (searchTimerRef.current) window.clearTimeout(searchTimerRef.current);
    searchTimerRef.current = window.setTimeout(() => {
      searchTimerRef.current = undefined;
      void runSearch(q);
    }, settings.search.debounceMs);
    return () => {
      if (searchTimerRef.current) {
        window.clearTimeout(searchTimerRef.current);
        searchTimerRef.current = undefined;
      }
    };
  }, [searchQ, settings.search.mode, settings.search.debounceMs, runSearch]);

  // — 单击：文件夹进、文件开 tab —
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

  /**
   * 把左侧树展开到指定目录。
   *
   * 树只存「已加载的 children」，所以定位到深度 N 必须逐级 listDir
   * （N 次串行目录枚举）——这是"跟随定位"的固有代价；不做的话树停在
   * 盘根，等于没定位。每一级的 key 一律取**后端返回的** e.path：树节点
   * 的 key 就是它，自己拼字符串会对不上（盘符根带尾反斜杠、UNC 前缀）。
   */
  const syncTreeTo = useCallback(
    async (dir: string) => {
      const fs = viewerFileSource.fs;
      if (!fs) return;
      const drive = drives.find((d) =>
        dir.toLowerCase().startsWith(`${d.letter.toLowerCase()}:`),
      );
      if (!drive) {
        // 驱动器列表还没回来（首屏并发）：记下来，等它到了再补一次。
        pendingSyncRef.current = dir;
        return;
      }
      const listDir = fs.listDir;
      let cur = drive.path;
      let entries = await listDir(cur, showHidden);
      setTree((t) => ({
        ...t,
        [cur]: {
          expanded: true,
          children: entries.ok ? entries.entries.filter((e) => e.isDir) : [],
        },
      }));
      for (const seg of dir.slice(drive.path.length).split(/[\\/]+/).filter(Boolean)) {
        if (!entries.ok) return;
        const hit = entries.entries.find(
          (e) => e.isDir && e.name.toLowerCase() === seg.toLowerCase(),
        );
        if (!hit) return;
        cur = hit.path;
        entries = await listDir(cur, showHidden);
        setTree((t) => ({
          ...t,
          [cur]: {
            expanded: true,
            children: entries.ok ? entries.entries.filter((e) => e.isDir) : [],
          },
        }));
      }
    },
    [drives, showHidden],
  );

  /**
   * 文件从**别处**被打开（拖入 / 主程序「在查看器中打开」/ 命令行 /
   * 单实例投递）时，中间栏与左侧树跟着定位到它实际所在的目录——
   * 否则右边开了个文件，左边还停在另一个盘，用户的空间感直接断掉。
   *
   * 纯净模式下不渲染树与中间栏，只把当前目录记下来（切到重量模式时
   * 目录已经是对的）；重量模式下同时逐级展开树。
   */
  const syncToFile = useCallback(
    async (filePath: string) => {
      const dir = await viewerFileSource.fs?.parent(filePath);
      if (!dir) return;
      if (mode === "heavy") {
        navigate(dir, false);
        void syncTreeTo(dir);
      } else {
        setCurrentPath(dir);
        setPathInput(dir);
      }
    },
    [mode, navigate, syncTreeTo],
  );

  /** 打开一个路径：目录进资源管理器（切重量模式），文件开预览标签。
   *  启动参数与单实例投递共用这一条判别——主程序「在查看器中打开」传目录
   *  是常规动作，走错分支会得到一个报「不是文件」的标签页。 */
  const openTarget = useCallback(
    async (path: string) => {
      const st = await viewerFileSource.fs?.stat(path);
      if (st?.isDir) {
        // 目录必须靠资源管理器看 → 切重量模式
        setMode("heavy");
        navigate(path, false);
      } else {
        openPath(path);
        void syncToFile(path);
      }
    },
    [navigate, openPath, syncToFile],
  );

  /**
   * 路径栏是**万能输入框**，按输入形态分派：
   *   1. 网址（http/https/www）→ 用系统浏览器打开；
   *   2. 绝对路径 → 目录进资源管理器，文件开预览标签（并跟着定位）；
   *   3. 其余 → 当搜索词（转交搜索）。
   *
   * 以前只有「目录」一种理解：传文件路径会报「不是目录」，用户只能
   * 自己在中间栏找。现在一个框能吃下三类输入。
   */
  const onPathSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      const v = pathInput.trim();
      if (!v) return;
      // 1. 网址
      if (/^(https?:\/\/|www\.)/i.test(v)) {
        const target = v.startsWith("www.") ? `https://${v}` : v;
        await viewerFileSource.openWithSystem?.(target);
        return;
      }
      // 2. 绝对路径（盘符 / UNC）
      if (/^[a-zA-Z]:[\\/]/.test(v) || v.startsWith("\\\\")) {
        const st = await viewerFileSource.fs?.stat(v);
        if (st?.isDir) {
          navigate(v, true);
          return;
        }
        if (st?.isFile) {
          await openTarget(v);
          return;
        }
        // 路径不存在：不当路径，当搜索词（用户多半想找它）
      }
      // 3. 搜索词
      setSearchQ(v);
      void runSearch(v);
    },
    [pathInput, navigate, openTarget, runSearch],
  );

  // 驱动器列表到位后，补做一次首屏期间排队的「跟随定位」。
  useEffect(() => {
    const target = pendingSyncRef.current;
    if (!target || drives.length === 0) return;
    pendingSyncRef.current = null;
    void syncTreeTo(target);
  }, [drives, syncTreeTo]);

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
        // 拖进来的可能是文件夹 —— 同样要走目录/文件判别，不能一律开成预览标签。
        if (any.path) void openTarget(any.path);
      }
    },
    [openTarget],
  );

  // 搜索
  const onSearchSubmit = useCallback(
    async (e: React.FormEvent) => {
      e.preventDefault();
      await runSearch(searchQ);
    },
    [runSearch, searchQ],
  );

  // —— 右键菜单：全局拦截原生右键 + 落点解析 ——
  //
  // 为什么在 document 上 capture 阶段统一拦截：WebView2 的原生右键菜单
  // 是 Chromium 那套（重新加载 / 查看源代码 / 检查），配色与本项目两套
  // 主题都不搭，而且给不了文件操作菜单。capture 阶段 preventDefault 之后，
  // 系统菜单对**任何**元素都不会出现——输入框、预览正文、HTML 沙箱都算。
  //
  // 落点靠 data-ctx 属性识别：组件只标属性，菜单内容由 contextMenuModel
  // 按 kind 决定，不必给每个组件挂 onContextMenu（少一层耦合）。
  const openMenu = useCallback(
    (
      x: number,
      y: number,
      target: MenuTarget,
      viaKeyboard = false,
      fieldEl?: Element | null,
    ) => {
      // 记下菜单打开时聚焦着的输入框：菜单一打开就会把焦点抢到第一项上，
      // 到时候再问 document.activeElement 已经问不到输入框了。右键落点
      // 本身就是输入框时以它为准（输入框不一定被浏览器聚焦过）。
      const pick = (e: Element | null | undefined) =>
        e && (e.tagName === "INPUT" || e.tagName === "TEXTAREA") && "value" in e
          ? (e as HTMLInputElement | HTMLTextAreaElement)
          : null;
      fieldRef.current = pick(fieldEl) ?? pick(document.activeElement);
      const f = fieldRef.current;
      const fieldSelection = f
        ? f.value.slice(f.selectionStart ?? 0, f.selectionEnd ?? 0)
        : "";
      setMenu({ x, y, target, viaKeyboard, fieldSelection });
    },
    [],
  );

  useEffect(() => {
    // 长按（触摸）阈值：与 Chromium 的长按判定一致。
    const LONG_PRESS_MS = 500;
    let timer: number | undefined;
    let origin = { x: 0, y: 0 };
    let lastLongPress = 0;
    /**
     * 「刚刚用键盘唤起了菜单」标记。
     *
     * Chrome 的 contextmenu 事件，鼠标右键与键盘菜单键都是 detail===0，
     * 分不出来——只能自己记：按过菜单键（ContextMenu）或 Shift+F10 才算。
     * 键盘唤起时要自动聚焦第一项（键盘用户直接用方向键走），
     * 鼠标唤起时不能聚焦（否则第一项顶着焦点环，像被选中）。
     */
    let keyboardInvoke = false;
    const onMenuKey = (e: KeyboardEvent) => {
      if (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10")) {
        keyboardInvoke = true;
      }
    };

    /**
     * 解析右键落点。
     *
     * 输入框自己就是落点，优先于向上找 data-ctx：.app 这类祖先也带
     * data-ctx="app"，靠 closest 会把搜索框 / AI 输入框的右键变成
     * 「应用菜单」，文本编辑（粘贴 / 复制选区 / 全选）全丢。
     */
    const ctxEl = (raw: Element | null) => {
      const own =
        raw instanceof HTMLElement &&
        (raw.tagName === "INPUT" || raw.tagName === "TEXTAREA" || raw.isContentEditable)
          ? raw
          : null;
      return { own, el: own ?? raw?.closest<HTMLElement>("[data-ctx]") ?? null };
    };

    const onContextMenu = (e: MouseEvent) => {
      e.preventDefault();
      // 长按已经开过菜单：Chromium 随后还会补一个 contextmenu 事件，
      // 不挡就会弹两次（第二份还盖在第一份上）。
      if (Date.now() - lastLongPress < 800) return;
      const { own, el } = ctxEl(e.target instanceof Element ? e.target : null);
      openMenu(e.clientX, e.clientY, resolveCtxTarget(el), keyboardInvoke, own);
      keyboardInvoke = false;
    };

    const onPointerDown = (e: PointerEvent) => {
      // 鼠标走 contextmenu 事件；只有触摸/手写笔才需要自己实现长按。
      if (e.pointerType === "mouse") return;
      const { own, el } = ctxEl(e.target instanceof Element ? e.target : null);
      if (!el) return;
      origin = { x: e.clientX, y: e.clientY };
      timer = window.setTimeout(() => {
        timer = undefined;
        lastLongPress = Date.now();
        openMenu(origin.x, origin.y, resolveCtxTarget(el), false, own);
      }, LONG_PRESS_MS);
    };
    const onPointerMove = (e: PointerEvent) => {
      if (timer === undefined) return;
      if (Math.abs(e.clientX - origin.x) > 10 || Math.abs(e.clientY - origin.y) > 10) {
        window.clearTimeout(timer);
        timer = undefined;
      }
    };
    const onPointerUp = () => {
      if (timer !== undefined) {
        window.clearTimeout(timer);
        timer = undefined;
      }
    };

    document.addEventListener("contextmenu", onContextMenu, true);
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("pointermove", onPointerMove, true);
    document.addEventListener("pointerup", onPointerUp, true);
    document.addEventListener("pointercancel", onPointerUp, true);
    document.addEventListener("keydown", onMenuKey, true);
    return () => {
      document.removeEventListener("contextmenu", onContextMenu, true);
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("pointermove", onPointerMove, true);
      document.removeEventListener("pointerup", onPointerUp, true);
      document.removeEventListener("pointercancel", onPointerUp, true);
      document.removeEventListener("keydown", onMenuKey, true);
    };
  }, [openMenu]);

  // 菜单开着时滚动页面：菜单是固定定位的，不跟着走——直接关掉。
  useEffect(() => {
    if (!menu) return undefined;
    const close = () => setMenu(null);
    document.addEventListener("scroll", close, true);
    return () => document.removeEventListener("scroll", close, true);
  }, [menu]);

  // 轻提示：动作结果必须看得见——静默成功等于没做。
  const notify = useCallback((text: string, kind: "ok" | "err" = "ok") => {
    setNotice({ text, kind });
    if (noticeTimer.current) window.clearTimeout(noticeTimer.current);
    noticeTimer.current = window.setTimeout(() => setNotice(null), 2600);
  }, []);

  const copyPath = useCallback(
    (path: string) => {
      if (!path) return;
      void viewerFileSource.copyText?.(path).then((r) => {
        if (!r) return;
        if (r.ok) notify("路径已复制");
        else notify(r.message ?? "复制失败", "err");
      });
    },
    [notify],
  );

  const copyText = useCallback(
    (text: string) => {
      if (!text) return;
      void viewerFileSource.copyText?.(text).then((r) => {
        if (!r) return;
        if (r.ok) notify("已复制选中文本");
        else notify(r.message ?? "复制失败", "err");
      });
    },
    [notify],
  );

  const pasteText = useCallback(() => {
    void viewerFileSource.pasteText?.().then((r) => {
      if (!r) return;
      if (!r.ok) {
        notify(r.message ?? "读不到剪贴板内容", "err");
        return;
      }
      setPathInput(r.text);
      notify("已粘贴到路径栏");
    });
  }, [notify]);

  /** 复制文件/文件夹到剪贴板（CF_HDROP，可直接粘到别的资源管理器窗口）。 */
  const copyFiles = useCallback(
    (paths: string[]) => {
      void viewerFileSource.copyFiles?.(paths).then((r) => {
        if (!r) return;
        if (r.ok) notify(`已复制 ${r.count ?? paths.length} 个项目`);
        else notify(r.message ?? "复制失败", "err");
      });
    },
    [notify],
  );

  /** 从剪贴板粘贴文件到当前目录（同名项跳过，不覆盖）。 */
  const pasteFiles = useCallback(
    (dir: string) => {
      void viewerFileSource.pasteFiles?.(dir).then((r) => {
        if (!r) return;
        if (!r.ok) {
          notify(r.message ?? "粘贴失败", "err");
          return;
        }
        const copied = r.copied?.length ?? 0;
        const skipped = r.skipped?.length ?? 0;
        notify(
          skipped
            ? `已粘贴 ${copied} 个，跳过 ${skipped} 个同名项`
            : `已粘贴 ${copied} 个项目`,
        );
        refresh();
      });
    },
    [notify, refresh],
  );

  /** 用指定程序打开（default / notepad / code / properties）。 */
  const openWith = useCallback(
    (path: string, app: string) => {
      void viewerFileSource.openWith?.(path, app).then((r) => {
        if (!r) return;
        if (!r.ok) notify(r.message ?? "打开失败", "err");
      });
    },
    [notify],
  );

  const showProperties = useCallback(
    (path: string) => {
      void viewerFileSource.openWith?.(path, "properties").then((r) => {
        if (!r) return;
        if (!r.ok) notify(r.message ?? "打不开属性", "err");
      });
    },
    [notify],
  );

  const reveal = useCallback(
    (path: string) => {
      void viewerFileSource.revealInExplorer?.(path).then((r) => {
        if (!r) return;
        if (!r.ok) notify(r.message ?? "无法在资源管理器中定位", "err");
      });
    },
    [notify],
  );

  /** 把文件交给 AI 右栏（开 tab + 带上引导语）。 */
  const addToAi = useCallback(
    (path: string, name: string) => {
      openPath(path);
      setAiOpen(true);
      setAiPending({ text: joinConversationPrompt(name), nonce: ++aiNonceRef.current });
    },
    [openPath],
  );

  /** 输入框的文本编辑动作（作用在菜单打开时聚焦的那个输入框上）。 */
  const fieldAction = useCallback(
    (action: "paste" | "copy" | "selectAll") => {
      const el = fieldRef.current;
      if (!el) return;
      if (action === "selectAll") {
        el.select();
        return;
      }
      if (action === "copy") {
        const start = el.selectionStart ?? 0;
        const end = el.selectionEnd ?? 0;
        const text = el.value.slice(start, end);
        if (!text) {
          notify("没有选中文字", "err");
          return;
        }
        copyText(text);
        return;
      }
      void viewerFileSource.pasteText?.().then((r) => {
        if (!r) return;
        if (!r.ok) {
          notify(r.message ?? "读不到剪贴板内容", "err");
          return;
        }
        const start = el.selectionStart ?? el.value.length;
        const end = el.selectionEnd ?? start;
        const next = el.value.slice(0, start) + r.text + el.value.slice(end);
        // React 在元素**实例**上劫持了 value（用它记「上一次的值」来判断
        // onChange 要不要触发）。直接 el.value = next，React 认为没变化、
        // onChange 不会触发——必须走原型上的原生 setter 绕过劫持。
        const setter = Object.getOwnPropertyDescriptor(
          Object.getPrototypeOf(el),
          "value",
        )?.set;
        if (setter) setter.call(el, next);
        else el.value = next;
        el.dispatchEvent(new Event("input", { bubbles: true }));
        const caret = start + r.text.length;
        el.setSelectionRange(caret, caret);
        notify("已粘贴");
      });
    },
    [copyText, notify],
  );

  // — 书签 —
  useEffect(() => {
    try {
      localStorage.setItem(BOOKMARKS_KEY, JSON.stringify(bookmarks));
    } catch {
      /* 持久化失败仅影响下次启动，本次会话内仍可用 */
    }
  }, [bookmarks]);

  const toggleBookmark = useCallback((entry: Pick<DirEntryLike, "path" | "name" | "isDir">) => {
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

  /**
   * 列表显示什么：**搜索进行中或已出结果** → 搜索结果；否则 → 目录内容。
   *
   * 曾经用 `searching` 判断，但 `searching` 是「请求在飞」的意思——搜索**完成**
   * 后它回到 false，于是「0 命中」的结果集被丢掉，列表又变回目录内容，
   * 用户看到的像是「搜索没生效」。判据必须是「有没有搜索结果集」。
   */
  const visibleEntries = searching || searchScope ? searchResults : entries;

  /**
   * 排序：后端已按「目录在前 + 名称」排好；按大小/时间排时前端再排一次。
   *
   * 目录永远在前（资源管理器惯例），同级内按选定键排。
   */
  const sortedEntries = useMemo(() => {
    if (sortBy === "name") return visibleEntries;
    const list = [...visibleEntries];
    list.sort((a, b) => {
      if (a.isDir !== b.isDir) return a.isDir ? -1 : 1;
      if (sortBy === "size") {
        return (b.size || 0) - (a.size || 0) || a.name.localeCompare(b.name, "zh");
      }
      const at = Date.parse(a.mtime ?? "") || 0;
      const bt = Date.parse(b.mtime ?? "") || 0;
      return bt - at || a.name.localeCompare(b.name, "zh");
    });
    return list;
  }, [visibleEntries, sortBy]);

  // 排序方式持久化。
  useEffect(() => {
    try {
      localStorage.setItem(SORT_KEY, sortBy);
    } catch {
      /* 持久化失败仅影响下次启动，本次会话内仍可用 */
    }
  }, [sortBy]);

  // 可用的「打开方式」：探测一次即可（装没装 VS Code 不会中途变）。
  useEffect(() => {
    let alive = true;
    void viewerFileSource.availableApps?.().then((apps) => {
      if (alive) setOpenApps(apps);
    });
    return () => {
      alive = false;
    };
  }, []);

  const closeOtherTabs = useCallback((id: number) => {
    setTabs((ts) => {
      for (const t of ts) if (t.id !== id) openedRef.current.delete(t.path);
      const rest = ts.filter((t) => t.id === id);
      setActiveTabId((cur) => (rest.some((t) => t.id === cur) ? cur : rest[0]?.id ?? null));
      return rest;
    });
  }, []);

  const closeAllTabs = useCallback(() => {
    setTabs([]);
    openedRef.current.clear();
    setActiveTabId(null);
  }, []);

  /** 菜单要用的状态与动作（一次算好，菜单打开时直接取）。 */
  const menuContext = useMemo(
    () => ({
      currentPath,
      activeFile: active ? { path: active.path, name: active.name } : null,
      sortBy,
      showHidden,
      mode,
      themeMode,
      openApps,
      isBookmarked: (path: string) => bookmarks.some((b) => b.path === path),
      tabCount: tabs.length,
      selection,
      fieldSelection: menu?.fieldSelection ?? "",
      canEdit: activeEditorKind !== "none",
      editLabel: editorLabel(activeEditorKind),
      edit: requestEdit,
      openEntry: (path: string, isDir: boolean) => {
        if (isDir) navigate(path, true);
        else openPath(path);
      },
      openWith,
      showProperties,
      reveal,
      copyPath,
      copyFiles,
      pasteFiles,
      copyText,
      pasteText,
      fieldAction,
      toggleBookmark: (path: string, name: string, isDir: boolean) =>
        toggleBookmark({ path, name, isDir }),
      addToAi,
      refresh,
      goUp: () => {
        void up();
      },
      closeTab: (id?: number) => {
        const target = id ?? activeTabId;
        if (target !== null && target !== undefined) closeTab(target);
      },
      closeOtherTabs,
      closeAllTabs,
      setSort: (kind: SortKind) => setSortBy(kind),
      setShowHidden: (visible: boolean) => setShowHidden(visible),
      setTheme,
      setMode: (m: ViewerMode) => switchMode(m),
    }),
    [
      active,
      activeTabId,
      addToAi,
      bookmarks,
      closeAllTabs,
      closeOtherTabs,
      closeTab,
      copyFiles,
      copyPath,
      copyText,
      currentPath,
      fieldAction,
      menu?.fieldSelection,
      mode,
      navigate,
      openApps,
      openPath,
      openWith,
      pasteFiles,
      pasteText,
      refresh,
      reveal,
      selection,
      setTheme,
      showHidden,
      showProperties,
      sortBy,
      switchMode,
      tabs.length,
      themeMode,
      toggleBookmark,
      up,
    ],
  );

  /**
   * 全局快捷键——菜单里写的提示必须真的能按。
   *
   * 输入框里让位（Ctrl+V 之类交给输入框自己处理）；F5 例外，刷新在
   * 任何地方都该生效。
   */
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        if (menu) {
          setMenu(null);
          return;
        }
        if (searchScope) exitSearch();
        return;
      }
      if (e.key === "F5") {
        e.preventDefault();
        refresh();
        return;
      }
      const t = e.target as HTMLElement | null;
      const inField =
        !!t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable);
      if (inField || menu) return;
      if (e.altKey && e.key === "ArrowLeft") {
        e.preventDefault();
        back();
      } else if (e.altKey && e.key === "ArrowRight") {
        e.preventDefault();
        forward();
      } else if (e.altKey && e.key === "ArrowUp") {
        e.preventDefault();
        void up();
      } else if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "c") {
        e.preventDefault();
        copyPath(currentPath ?? "");
      } else if (e.ctrlKey && e.key.toLowerCase() === "w") {
        e.preventDefault();
        if (activeTabId !== null) closeTab(activeTabId);
      } else if (e.ctrlKey && e.key.toLowerCase() === "v") {
        e.preventDefault();
        if (currentPath) pasteFiles(currentPath);
      } else if (e.ctrlKey && e.key.toLowerCase() === "e") {
        // 编辑当前文件：菜单里写着 Ctrl+E，就必须真能按。
        e.preventDefault();
        if (activeEditorKind !== "none") requestEdit();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [
    activeEditorKind,
    activeTabId,
    back,
    closeTab,
    copyPath,
    currentPath,
    exitSearch,
    forward,
    menu,
    pasteFiles,
    refresh,
    requestEdit,
    searchScope,
    up,
  ]);

  // 标题栏副标题：纯净模式看当前文件，重量模式看当前目录。
  const titleSubject =
    mode === "pure"
      ? active?.name
      : currentPath
        ? (splitPath(currentPath).pop() ?? currentPath)
        : undefined;
  const titleSubjectFull = mode === "pure" ? active?.path : (currentPath ?? undefined);

  return (
    <div
      className="app"
      style={layoutVars}
      data-ctx="app"
      onDragOver={(e) => {
        e.preventDefault();
        setDragOver(true);
      }}
      onDragLeave={() => setDragOver(false)}
      onDrop={onDrop}
    >
      <TitleBar
        title="Hermix 文件查看器"
        subject={titleSubject}
        subjectTitle={titleSubjectFull}
        menus={menus}
        onMenuCommand={onMenuCommand}
      />
      {sysOpen && <SystemPanel onClose={() => setSysOpen(false)} />}
      {aboutOpen && <AboutDialog onClose={() => setAboutOpen(false)} />}
      <SettingsDialog
        open={settingsOpen}
        onClose={() => setSettingsOpen(false)}
        onSaved={(message) => {
          void loadSettings().then(setSettings);
          notify(message);
        }}
      />
      {mode === "heavy" && (
      <header className="toolbar">
        <button className="tb-btn" onClick={back} disabled={backStack.length === 0} title="后退" aria-label="后退">
          <ArrowLeft size={16} />
        </button>
        <button
          className="tb-btn"
          onClick={forward}
          disabled={forwardStack.length === 0}
          title="前进"
          aria-label="前进"
        >
          <ArrowRight size={16} />
        </button>
        <button className="tb-btn" onClick={up} disabled={!currentPath} title="上一级" aria-label="上一级">
          <ArrowUp size={16} />
        </button>
        <button className="tb-btn" onClick={refresh} title="刷新" aria-label="刷新">
          <RefreshCw size={16} />
        </button>
        <form className="path-form" onSubmit={onPathSubmit}>
          <input
            className="path-input"
            value={pathInput}
            onChange={(e) => setPathInput(e.target.value)}
            placeholder="路径 / 网址 / 搜索…"
            data-ctx="pathbar"
          />
          <button type="submit" className="tb-btn" title="跳转" aria-label="跳转">→</button>
        </form>
        <div className="toolbar-spacer" />
        {/* 视图选项组：三个都是「开关/切换」，光图标用户看不懂
            （"那个眼睛是干啥的"）——一律配文字标签。 */}
        <div className="tb-group">
          <button
            className={`tb-btn ${showHidden ? "is-active" : ""}`}
            onClick={() => setShowHidden((v) => !v)}
            title={
              showHidden
                ? "正在显示带「隐藏」属性的文件，点击改为隐藏"
                : "正在隐藏带「隐藏」属性的文件，点击改为显示"
            }
          >
            {/* 漏斗而不是眼睛：眼睛是通用「可见性」语义，用户会以为管的是
                预览的显隐（"那个眼睛是干啥的"）；漏斗说的是「这份列表被
                筛了没有」，正好是这个开关干的事。 */}
            {showHidden ? <Funnel size={16} /> : <FunnelX size={16} />}
            隐藏文件
          </button>
          <button
            className={`tb-btn ${aiOpen ? "is-active" : ""}`}
            onClick={() => setAiOpen((v) => !v)}
            title={aiOpen ? "收起 AI 栏" : "展开 AI 栏"}
          >
            <Sparkles size={16} /> AI
          </button>
          <button className="tb-btn" onClick={cycleTheme} title={`主题：${THEME_LABEL[themeMode]}`}>
            {themeMode === "system" ? (
              <Monitor size={16} />
            ) : themeMode === "light" ? (
              <Sun size={16} />
            ) : (
              <Moon size={16} />
            )}
            {THEME_LABEL[themeMode]}
          </button>
        </div>
      </header>
      )}

      {mode === "heavy" && (
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
              placeholder="搜索文件…"
            />
            {searching && (
              <button
                type="button"
                className="search-clear"
                onClick={exitSearch}
                aria-label="退出搜索"
              >
                <X />
              </button>
            )}
          </form>
        )}
        {/* 搜索范围：全盘索引 vs 当前目录——两者结果差几个数量级，
            用户必须一眼看出「搜到的是不是全盘」。
            搜索**进行中**也要有反馈（首次搜索可能要等 UAC + 索引装载，
            几秒到几十秒不等），所以条件是 searchNote 而不是 searchScope
            （scope 要等结果回来才定）。 */}
        {searchNote && (
          <span
            className={`search-scope ${searchScope === "global" ? "is-global" : ""}`}
            title={searchNote}
          >
            {searchNote}
          </span>
        )}
      </nav>
      )}

      {mode === "pure" && (
        <header className="pure-bar">
          {active ? (
            <>
              <FileTypeIcon name={active.name} size={16} />
              <span className="pure-name" title={active.path}>
                {active.name}
              </span>
            </>
          ) : (
            <span className="pure-name">未打开文件</span>
          )}
          <div className="toolbar-spacer" />
          <button
            className="tb-btn"
            disabled={!active}
            onClick={() => {
              if (active) void viewerFileSource.openWithSystem?.(active.path);
            }}
            title="用系统默认程序打开"
          >
            <ExternalLink />
            系统打开
          </button>
          <button
            className="tb-btn"
            onClick={() => switchMode("heavy")}
            title="切到重量模式（资源管理器 + AI 协助）"
          >
            <HardDrive />
            重量模式
          </button>
        </header>
      )}

      <div className="body-row">
        {mode === "heavy" && (
        <>
        <aside className="sidebar">
          {bookmarks.length > 0 && (
            <>
              <div className="sb-head">书签</div>
              {bookmarks.map((b) => (
                <div
                  key={b.path}
                  className={`sb-row ${currentPath === b.path ? "is-active" : ""}`}
                  data-ctx="entry"
                  data-ctx-path={b.path}
                  data-ctx-name={b.name}
                  data-ctx-dir={b.isDir ? "1" : "0"}
                  data-ctx-from="bookmark"
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

        <section className="explorer" data-ctx="explorer">
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
                      {searching ? "正在搜索…" : searchScope ? "无搜索结果" : "空目录"}
                    </td>
                  </tr>
                )}
                {sortedEntries.map((e) => (
                  <tr
                    key={e.path}
                    className={`entry-row ${e.isDir ? "is-dir" : ""} ${
                      active?.path === e.path ? "is-active" : ""
                    }`}
                    onClick={(ev) => {
                      // 单击即打开：文件开右侧预览标签，目录进入。
                      // 只认第一次点击（detail === 1）：双击的第二次落在
                      // 「已进入的新目录」里的某一行上时会被这里挡掉，否则
                      // 双击目录会顺手把新目录里的文件误开一个。
                      if (ev.detail !== 1) return;
                      openEntry(e);
                    }}
                    title={e.path}
                    data-ctx="entry"
                    data-ctx-path={e.path}
                    data-ctx-name={e.name}
                    data-ctx-dir={e.isDir ? "1" : "0"}
                    data-ctx-from="list"
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
        </>
        )}

        <section className={`preview ${mode === "pure" ? "is-pure" : ""}`}>
          {/* 标签条只在有标签时出现：空着的时候留一条灰条 + 一句
              "单击文件即可预览"，和下面的空状态说的是同一件事（同一屏
              说两遍）。空状态的文案已经覆盖了"单击"和"拖入"。 */}
          {tabs.length > 0 && (
          <div className="tabbar">
            {tabs.map((t) => (
              <div
                key={t.id}
                className={`tab ${t.id === activeTabId ? "active" : ""}`}
                onClick={() => setActiveTabId(t.id)}
                title={t.path}
                data-ctx="tab"
                data-ctx-id={t.id}
                data-ctx-path={t.path}
                data-ctx-name={t.name}
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
            <div className="tabbar-spacer" />
          </div>
          )}
          <div
            className="preview-body"
            data-ctx="preview"
            data-ctx-path={active?.path ?? ""}
            data-ctx-name={active?.name ?? ""}
          >
            {!active ? (
              <div className="empty">
                <div className="empty-icon">
                  <FileSearch size={40} strokeWidth={1.5} />
                </div>
                <p>在左侧文件列表单击文件即可预览</p>
                <p className="empty-sub">
                  也可直接拖入文件 · 支持 Markdown / 文本 / 代码 / 图片 / PDF / Office / HTML
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
                  canEdit={mode === "heavy"}
                  editRequest={editRequest}
                />
              </Suspense>
            )}
          </div>
        </section>

        {mode === "heavy" && aiOpen && (
          <>
            <Resizer onDelta={onAiDelta} label="调整 AI 栏宽度" />
            <AiSidebar
              // key 绑当前文件：切换标签时重建面板 → 会话 id 随之重置。
              // 否则不关 AI 栏换文件，新文件路径会被拼进上一个文件的会话里。
              key={active?.path ?? "__none__"}
              filePath={active?.path ?? null}
              selection={selection}
              pending={aiPending}
              onPendingHandled={clearAiPending}
              onClose={() => setAiOpen(false)}
            />
          </>
        )}
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          groups={buildMenu(menu.target, menuContext)}
          autoFocusFirst={menu.viaKeyboard}
          onClose={() => setMenu(null)}
        />
      )}

      {notice && (
        <div className={`toast ${notice.kind === "err" ? "is-err" : ""}`} role="status">
          {notice.text}
        </div>
      )}

      {dragOver && <div className="drop-overlay">松开以打开文件</div>}
    </div>
  );
}
