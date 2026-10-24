// 文件搜索小窗（独立 window：index.html#/file-search）。
// Alt+F 全局热键唤起（file_search_controller），搜索引擎是提权守护进程
// hermix-filesearch.exe，本组件只做展示与交互：
//   搜索/筛选 → GET /v1/filesearch/search（stdio 桥转发 → 回环 TCP → helper）
//   状态轮询  → GET /v1/filesearch/status（卷同步状态 / 跳过清单）
//   过滤规则  → POST /v1/filesearch/filter（持久化在 localStorage，启动回放）
//   双击打开  → invoke("open_in_viewer")（hermix-viewer.exe，单实例投递）

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { fetchJSON } from "../api";
import { SAVE } from "./strings";

// ── 类型（对应 hermix-filesearch core/model.rs 的 serde camelCase 输出） ──

interface FileEntry {
  name: string;
  path: string;
  extension: string | null;
  size: number | null;
  modified: number;
  isDir: boolean;
  volume: string;
}

interface SearchResponse {
  totalMatches: number;
  returned: number;
  results: FileEntry[];
  elapsedMs: number;
}

interface VolumeStatus {
  volume: string | null;
  totalEntries: number;
  isIndexed: boolean;
  snapshotLoaded: boolean;
  needsRebuild: boolean;
  statusMessage: string | null;
  monitorState: string; // idle | watching | syncing | needsRebuild | error
  lastSyncAt: number | null;
  lastSyncError: string | null;
}

interface StatusResponse {
  volumes: VolumeStatus[];
  skipped: Record<string, string>;
}

type Category = "all" | "dir" | "doc" | "img";

const DOC_EXTS = new Set([
  "doc", "docx", "pdf", "txt", "md", "xls", "xlsx", "ppt", "pptx", "csv", "rtf", "odt", "epub",
]);
const IMG_EXTS = new Set([
  "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "tif", "tiff", "heic",
]);

const CONFIG_KEY = "hermix.filesearch.config";
const ROW_HEIGHT = 32;
const OVERSCAN = 10;

// ── 格式化辅助 ──

function formatSize(size: number | null): string {
  if (size === null || size === undefined) return "—";
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  if (size < 1024 * 1024 * 1024) return `${(size / 1024 / 1024).toFixed(1)} MB`;
  return `${(size / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatTime(ms: number): string {
  if (!ms) return "—";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function extOf(entry: FileEntry): string {
  return entry.extension?.toLowerCase() ?? "";
}

// ── 组件 ──

export function FileSearchWindow() {
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<Category>("all");
  const [results, setResults] = useState<FileEntry[]>([]);
  const [totalMatches, setTotalMatches] = useState(0);
  const [elapsedMs, setElapsedMs] = useState(0);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [slowStart, setSlowStart] = useState(false); // 请求超 1.5s（多半在等 UAC）
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [hotkey, setHotkey] = useState("Alt+F");
  const [excludedDirs, setExcludedDirs] = useState("");
  const [excludedExts, setExcludedExts] = useState("");
  const [skipHiddenSystem, setSkipHiddenSystem] = useState(true);

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [scrollTop, setScrollTop] = useState(0);
  const [viewportH, setViewportH] = useState(600);
  const abortRef = useRef<AbortController | null>(null);
  const seqRef = useRef(0);

  // mount：亮窗（Rust 先建后亮）+ 聚焦输入框 + 启动状态轮询。
  useEffect(() => {
    void getCurrentWindow().show();
    inputRef.current?.focus();
    try {
      const raw = localStorage.getItem(CONFIG_KEY);
      if (raw) {
        const cfg = JSON.parse(raw) as {
          hotkey?: string;
          filter?: { excludedDirs?: string[]; excludedExtensions?: string[]; skipHiddenSystem?: boolean };
        };
        if (cfg.hotkey) setHotkey(cfg.hotkey);
        if (cfg.filter) {
          setExcludedDirs((cfg.filter.excludedDirs ?? []).join(", "));
          setExcludedExts((cfg.filter.excludedExtensions ?? []).join(", "));
          setSkipHiddenSystem(cfg.filter.skipHiddenSystem ?? true);
        }
      }
    } catch { /* ignore */ }
  }, []);

  useEffect(() => {
    let stopped = false;
    const tick = async () => {
      try {
        const s = await fetchJSON<StatusResponse>("/v1/filesearch/status");
        if (!stopped) setStatus(s);
      } catch {
        // helper 未运行 / 握手中：状态栏已有文案，不刷屏。
      }
    };
    void tick();
    const timer = window.setInterval(tick, 2000);
    return () => {
      stopped = true;
      window.clearInterval(timer);
    };
  }, []);

  // 搜索：150ms 防抖 + 请求序号防乱序 + abort 上一发。
  useEffect(() => {
    const seq = ++seqRef.current;
    const timer = window.setTimeout(async () => {
      abortRef.current?.abort();
      const controller = new AbortController();
      abortRef.current = controller;
      setSearching(true);
      setSearchError(null);
      const slowTimer = window.setTimeout(() => setSlowStart(true), 1500);
      try {
        const url = `/v1/filesearch/search?query=${encodeURIComponent(query)}&limit=300`;
        const resp = await fetchJSON<SearchResponse>(url, { signal: controller.signal });
        if (seqRef.current !== seq) return; // 已有更新的请求
        setResults(resp.results ?? []);
        setTotalMatches(resp.totalMatches ?? 0);
        setElapsedMs(resp.elapsedMs ?? 0);
      } catch (err) {
        if (seqRef.current !== seq || controller.signal.aborted) return;
        setResults([]);
        setTotalMatches(0);
        setSearchError(err instanceof Error ? err.message : String(err));
      } finally {
        window.clearTimeout(slowTimer);
        if (seqRef.current === seq) {
          setSearching(false);
          setSlowStart(false);
        }
      }
    }, 150);
    return () => window.clearTimeout(timer);
  }, [query]);

  // 分类过滤（客户端；helper 已按扩展名/目录位过滤系统文件）。
  const filtered = useMemo(() => {
    if (category === "all") return results;
    return results.filter((e) => {
      if (category === "dir") return e.isDir;
      const ext = extOf(e);
      if (category === "doc") return !e.isDir && DOC_EXTS.has(ext);
      return !e.isDir && IMG_EXTS.has(ext);
    });
  }, [results, category]);

  // 虚拟滚动：定高行 + overscan。
  const visible = useMemo(() => {
    const start = Math.max(0, Math.floor(scrollTop / ROW_HEIGHT) - OVERSCAN);
    const end = Math.min(filtered.length, Math.ceil((scrollTop + viewportH) / ROW_HEIGHT) + OVERSCAN);
    return { start, end };
  }, [scrollTop, viewportH, filtered.length]);

  const onScroll = useCallback(() => {
    const el = listRef.current;
    if (el) setScrollTop(el.scrollTop);
  }, []);

  // 表格高度自适应测量（虚拟窗口高度）。
  useEffect(() => {
    const el = listRef.current;
    if (!el) return;
    const observer = new ResizeObserver(() => setViewportH(el.clientHeight));
    observer.observe(el);
    setViewportH(el.clientHeight);
    return () => observer.disconnect();
  }, []);

  // ESC：隐藏窗口（关窗=隐藏，热键秒回）。
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") void getCurrentWindow().hide();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const openEntry = useCallback((entry: FileEntry) => {
    void invoke("open_in_viewer", { path: entry.path }).catch((err) => {
      console.warn("[filesearch] 打开失败:", err);
    });
  }, []);

  const saveSettings = useCallback(async () => {
    const filter = {
      excludedDirs: excludedDirs.split(/[,，\n]/).map((s) => s.trim().toLowerCase()).filter(Boolean),
      excludedExtensions: excludedExts.split(/[,，\n]/).map((s) => s.trim().toLowerCase().replace(/^\./, "")).filter(Boolean),
      skipHiddenSystem,
    };
    localStorage.setItem(
      CONFIG_KEY,
      JSON.stringify({ enabled: true, hotkey: hotkey.trim() || "alt+F", filter }),
    );
    const hk = hotkey.trim() || "alt+F";
    try {
      // Rust 侧被占用时会自动换备选键并回报 substituted —— 那不是失败，
      // 要把"现在按的是哪个键"告诉用户，否则用户以为没生效。
      const st = await invoke<{ active: string | null; substituted: boolean }>(
        "file_search_apply_config",
        { enabled: true, hotkey: hk },
      );
      if (st.substituted) {
        setSearchError(`热键 ${hk} 被其他程序占用，已自动改用 ${st.active}`);
      }
      await fetchJSON("/v1/filesearch/filter", { method: "POST", body: JSON.stringify(filter) });
      setSettingsOpen(false);
    } catch (err) {
      setSearchError(err instanceof Error ? err.message : String(err));
    }
  }, [hotkey, excludedDirs, excludedExts, skipHiddenSystem]);

  // ── 状态栏文案 ──
  const statusBar = useMemo(() => {
    if (!status) {
      return { tone: "idle" as const, text: "连接索引服务中…（如弹出授权窗口请确认）" };
    }
    const volumes = status.volumes;
    const totalEntries = volumes.reduce((acc, v) => acc + (v.totalEntries || 0), 0);
    const skippedCount = Object.keys(status.skipped).length;
    if (volumes.some((v) => v.monitorState === "syncing" || v.needsRebuild)) {
      return { tone: "busy" as const, text: `正在同步索引（${volumes.length} 卷，${totalEntries.toLocaleString()} 项）` };
    }
    if (volumes.length === 0) {
      return { tone: "busy" as const, text: "索引服务已连接，等待卷加载…" };
    }
    if (volumes.every((v) => v.monitorState === "watching")) {
      return {
        tone: "ok" as const,
        text: `实时同步中 · ${volumes.length} 卷 · ${totalEntries.toLocaleString()} 项` + (skippedCount ? ` · 跳过 ${skippedCount} 卷` : ""),
      };
    }
    const err = volumes.find((v) => v.monitorState === "error" || v.lastSyncError);
    return {
      tone: "warn" as const,
      text: err?.lastSyncError ?? err?.statusMessage ?? "部分卷未同步",
    };
  }, [status]);

  const rows = filtered.slice(visible.start, visible.end);

  return (
    <div className="fsw-root">
      <header className="fsw-titlebar" data-tauri-drag-region>
        <span className="fsw-title" data-tauri-drag-region>文件搜索</span>
        <span className="fsw-hotkey-chip">{hotkey.replace(/[+]/g, " + ")}</span>
        <div className="fsw-titlebar-actions">
          <button
            className="fsw-icon-btn"
            title="设置"
            onClick={() => setSettingsOpen((v) => !v)}
          >
            ⚙
          </button>
          <button
            className="fsw-icon-btn fsw-close"
            title="隐藏（Alt+F 唤回）"
            onClick={() => void getCurrentWindow().hide()}
          >
            ✕
          </button>
        </div>
      </header>

      <div className="fsw-search-row">
        <input
          ref={inputRef}
          className="fsw-input"
          placeholder="搜索全盘文件…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          spellCheck={false}
        />
        <span className="fsw-count" aria-busy={searching}>
          {searching
            ? "搜索中…"
            : totalMatches > filtered.length
              ? `命中 ${totalMatches.toLocaleString()} · 显示 ${filtered.length.toLocaleString()} · ${elapsedMs} ms`
              : `${filtered.length.toLocaleString()} 项 · ${elapsedMs} ms`}
        </span>
      </div>

      <div className="fsw-filter-row">
        {(
          [
            ["all", "全部"],
            ["dir", "仅文件夹"],
            ["doc", "文档"],
            ["img", "图片"],
          ] as const
        ).map(([key, label]) => (
          <button
            key={key}
            className={`fsw-chip${category === key ? " active" : ""}`}
            onClick={() => setCategory(key)}
          >
            {label}
          </button>
        ))}
      </div>

      {settingsOpen && (
        <div className="fsw-settings">
          <label className="fsw-setting-row">
            <span>全局热键</span>
            <input
              className="fsw-setting-input"
              value={hotkey}
              onChange={(e) => setHotkey(e.target.value)}
              placeholder="alt+F"
              spellCheck={false}
            />
          </label>
          <label className="fsw-setting-row">
            <span>排除目录（逗号分隔，任意层级生效）</span>
            <input
              className="fsw-setting-input"
              value={excludedDirs}
              onChange={(e) => setExcludedDirs(e.target.value)}
              placeholder="$recycle.bin, node_modules, .git, target"
              spellCheck={false}
            />
          </label>
          <label className="fsw-setting-row">
            <span>排除扩展名（逗号分隔，不带点）</span>
            <input
              className="fsw-setting-input"
              value={excludedExts}
              onChange={(e) => setExcludedExts(e.target.value)}
              placeholder="tmp, log, bak"
              spellCheck={false}
            />
          </label>
          <label className="fsw-setting-row fsw-setting-check">
            <input
              type="checkbox"
              checked={skipHiddenSystem}
              onChange={(e) => setSkipHiddenSystem(e.target.checked)}
            />
            <span>跳过隐藏 + 系统文件（pagefile.sys、回收站元数据等）</span>
          </label>
          <p className="fsw-setting-note">规则保存后立即下发；改目录/扩展名会让对应卷在下次启动时按新规则重建。</p>
          <button className="fsw-save-btn" onClick={() => void saveSettings()}>
            {SAVE}
          </button>
        </div>
      )}

      <div className="fsw-header-row">
        <span className="fsw-col-name">名称</span>
        <span className="fsw-col-path">路径</span>
        <span className="fsw-col-size">大小</span>
        <span className="fsw-col-time">修改时间</span>
      </div>

      <div className="fsw-list" ref={listRef} onScroll={onScroll}>
        {slowStart && (
          <div className="fsw-hint">首次使用需建立索引：如弹出授权窗口请点「是」，然后稍候（全盘枚举约 1-3 分钟，之后秒开）。</div>
        )}
        {searchError && <div className="fsw-hint fsw-hint-error">{searchError}</div>}
        {!searchError && filtered.length === 0 && !searching && (
          <div className="fsw-hint">{query ? "没有匹配的文件" : "输入关键字开始搜索"}</div>
        )}
        <div style={{ height: visible.start * ROW_HEIGHT }} />
        {rows.map((entry, i) => (
          <div
            key={`${entry.volume}-${entry.path}-${visible.start + i}`}
            className="fsw-row"
            onDoubleClick={() => openEntry(entry)}
            title={`${entry.path}\n双击：用 hermix 查看器打开`}
          >
            <span className="fsw-col-name">
              <span className={`fsw-icon${entry.isDir ? " dir" : ""}`}>{entry.isDir ? "📁" : "📄"}</span>
              <span className="fsw-name-text">{entry.name}</span>
            </span>
            <span className="fsw-col-path">{entry.path}</span>
            <span className="fsw-col-size">{formatSize(entry.size)}</span>
            <span className="fsw-col-time">{formatTime(entry.modified)}</span>
          </div>
        ))}
        <div style={{ height: Math.max(0, (filtered.length - visible.end) * ROW_HEIGHT) }} />
      </div>

      <footer className={`fsw-statusbar tone-${statusBar.tone}`}>
        <span className={`fsw-dot ${statusBar.tone}`} />
        <span className="fsw-status-text">{statusBar.text}</span>
        {status && Object.keys(status.skipped).length > 0 && (
          <span
            className="fsw-status-skipped"
            title={Object.entries(status.skipped)
              .map(([vol, reason]) => `${vol}: ${reason}`)
              .join("\n")}
          >
            跳过：{Object.keys(status.skipped).join("、")}
          </span>
        )}
        <span className="fsw-status-tip">双击：用 hermix 查看器打开</span>
      </footer>
    </div>
  );
}
