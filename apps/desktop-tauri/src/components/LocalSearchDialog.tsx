import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { clsx } from "clsx";
import {
  Search, X, Loader2, Folder, File, FolderOpen, Eye, ExternalLink, FileText, AlertTriangle,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

/**
 * 本地文件查找面板（Windows 资源管理器样式）。
 *
 * 结构照抄 Explorer 的「详细信息」视图自上而下四段：
 *   标题栏 → 工具栏（无边框搜索框 + 类型筛选）→ 列头（名称/修改日期/
 *   类型/大小）→ 结果列表，底部一条状态栏（「N 个项目」+ 索引进度）。
 *
 * 搜索框是 Explorer 的灰底圆角胶囊：**任何状态都没有边框线**，焦点靠底色
 * 深浅区分（focus-within 换 bg-bg-active）。这是按用户要求做的显式取舍 ——
 * 焦点提示从描边改成了底色，代价是没有 outline 兜底。
 *
 * 选中行用中性底（bg-bg-active）而非实底强调色：暗色主题下 accent 是浅绿
 * （#6fae94），实底压文字需要动态的 on-accent，当前 token 做不到，会撞
 * WCAG 对比度。激活的筛选 chip 走描边 + accent 文字，既有品牌色又不需要
 * 压文字。
 *
 * 预览栏 = Explorer 的详细信息窗格，选中行才展开。
 *
 * 索引状态与查询结果是两条独立的通道：查询走 local_search（带匹配），
 * 进度走 local_search_status（只读计数器）。后台扫描期间每 500ms 轮询
 * 一次后者，「已发现 N 项」才会真的往上跳。设计依据见
 * docs/notes/implemented/architecture/2026-10-04-local-index-startup-and-progress-channel.md
 */

interface LocalSearchHit {
  path: string;
  name: string;
  is_dir: boolean;
  size: number | null;
  modified_secs: number | null;
}

interface LocalSearchResponse {
  hits: LocalSearchHit[];
  indexed: number;
  building: boolean;
}

/** 索引进度（local_search_status 的返回）。 */
interface IndexStatus {
  indexed: number;
  building: boolean;
  /** 提权 helper 最近一次尝试的结果：unauthorized/failed 时出提示条。 */
  helper_state?: "ok" | "unauthorized" | "failed";
}

type PreviewPayload =
  | { kind: "image"; data_url: string }
  | { kind: "text"; text: string; truncated: boolean }
  | { kind: "none"; reason: string };

/** 预设快捷筛选：一组扩展名勾选块（单选，再点取消）。 */
const EXT_PRESETS: { label: string; ext: string }[] = [
  { label: "PDF", ext: "pdf" },
  { label: "Word", ext: "docx" },
  { label: "Excel", ext: "xlsx" },
  { label: "PPT", ext: "pptx" },
  { label: "文本", ext: "txt" },
  { label: "Markdown", ext: "md" },
  { label: "图片", ext: "png" },
  { label: "压缩包", ext: "zip" },
];

/** 「类型」列的显示名。Explorer 会显示「PDF 文档」这类友好描述。 */
const TYPE_LABELS: Record<string, string> = {
  pdf: "PDF 文档",
  doc: "Word 文档", docx: "Word 文档",
  xls: "Excel 工作表", xlsx: "Excel 工作表",
  ppt: "PowerPoint 演示文稿", pptx: "PowerPoint 演示文稿",
  txt: "文本文档", md: "Markdown 文档", markdown: "Markdown 文档",
  png: "PNG 图像", jpg: "JPEG 图像", jpeg: "JPEG 图像", gif: "GIF 图像",
  webp: "WebP 图像", bmp: "位图图像", svg: "SVG 图像", ico: "图标",
  zip: "压缩文件", rar: "压缩文件", "7z": "压缩文件",
  rs: "Rust 源文件", py: "Python 文件", ts: "TypeScript 文件", js: "JavaScript 文件",
  json: "JSON 文件", toml: "TOML 文件", yaml: "YAML 文件", yml: "YAML 文件",
  log: "日志文件", csv: "CSV 文件", html: "HTML 文档", css: "CSS 样式表",
  xml: "XML 文档", exe: "应用程序", dll: "扩展名",
};

function typeLabel(name: string, isDir: boolean): string {
  if (isDir) return "文件夹";
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return "文件";
  const ext = name.slice(dot + 1).toLowerCase();
  return TYPE_LABELS[ext] ?? `${ext.toUpperCase()} 文件`;
}

function formatSize(bytes: number | null): string {
  if (bytes == null) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** Explorer 的日期列：今天显示时间，今年显示「M月D日」，跨年补年份。 */
function formatTime(secs: number | null): string {
  if (secs == null || secs <= 0) return "";
  const d = new Date(secs * 1000);
  const now = new Date();
  const hm = d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  if (d.toDateString() === now.toDateString()) return `${hm}`;
  if (d.getFullYear() === now.getFullYear()) {
    return d.toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" });
  }
  return d.toLocaleDateString("zh-CN", { year: "numeric", month: "2-digit", day: "2-digit" });
}

/** 千分位：状态栏「1,234 个项目」。 */
function formatCount(n: number): string {
  return n.toLocaleString("en-US");
}

/** ── 右侧预览（= Explorer 详细信息窗格）──────────────────────── */

function PreviewPane({ hit }: { hit: LocalSearchHit | null }) {
  const [preview, setPreview] = useState<PreviewPayload | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!hit || hit.is_dir) {
      setPreview(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setPreview(null);
    invoke<PreviewPayload>("local_search_preview", { path: hit.path })
      .then((p) => {
        if (!cancelled) setPreview(p);
      })
      .catch(() => {
        if (!cancelled) setPreview({ kind: "none", reason: "预览读取失败" });
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [hit]);

  if (!hit) {
    return (
      <div className="flex flex-col items-center justify-center gap-2 py-16 text-text-faint">
        <Eye className="icon-md" />
        <p className="text-meta">选中左侧结果进行预览</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col min-h-0 h-full">
      {/* 预览头：文件名 + 打开按钮 */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-border-primary shrink-0">
        <span className="flex-1 truncate-1 text-meta font-medium text-text-primary" title={hit.path}>
          {hit.name}
        </span>
        <button
          type="button"
          className="icon-btn shrink-0"
          title="打开"
          onClick={() => invoke("local_search_open", { path: hit.path }).catch(() => {})}
        >
          <ExternalLink className="icon-sm" />
        </button>
      </div>
      {/* 预览体 */}
      <div className="flex-1 min-h-0 overflow-auto">
        {loading ? (
          <div className="flex items-center justify-center py-16 text-text-faint">
            <Loader2 className="icon-md animate-spin" />
          </div>
        ) : !preview || hit.is_dir ? (
          <div className="flex flex-col items-center justify-center gap-2 py-16 text-text-faint">
            {hit.is_dir ? <Folder className="icon-md" /> : <File className="icon-md" />}
            <p className="text-meta">{hit.is_dir ? "文件夹 · 双击打开" : "无预览"}</p>
          </div>
        ) : preview.kind === "image" ? (
          <div className="flex items-center justify-center p-4">
            <img
              src={preview.data_url}
              alt={hit.name}
              className="max-w-full max-h-full object-contain rounded-md"
            />
          </div>
        ) : preview.kind === "text" ? (
          <div className="p-3">
            <pre className="whitespace-pre-wrap break-all text-tiny leading-relaxed text-text-secondary font-mono">
              {preview.text}
            </pre>
            {preview.truncated && (
              <p className="text-tiny text-text-faint mt-2 pt-2 border-t border-border-primary">
                内容已截断（超过 256 KB），打开查看完整文件
              </p>
            )}
          </div>
        ) : (
          <div className="flex flex-col items-center justify-center gap-2 py-16 text-text-faint px-6 text-center">
            {preview.kind === "none" && <FileText className="icon-md" />}
            <p className="text-meta">
              {preview.kind === "none" ? preview.reason : "无预览"}
            </p>
            {preview.kind === "none" && (
              <button
                type="button"
                className="mt-1 px-3 py-1 rounded-md text-meta border border-border-secondary text-text-secondary hover:bg-bg-tertiary transition-colors"
                onClick={() => invoke("local_search_open", { path: hit.path }).catch(() => {})}
              >
                打开文件
              </button>
            )}
          </div>
        )}
      </div>
    </div>
  );
}

/** ── 主面板 ───────────────────────────────────────────────── */

export function LocalSearchDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [query, setQuery] = useState("");
  const [ext, setExt] = useState<string | null>(null);
  const [dirsOnly, setDirsOnly] = useState(false);
  const [resp, setResp] = useState<LocalSearchResponse | null>(null);
  const [status, setStatus] = useState<IndexStatus | null>(null);
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<LocalSearchHit | null>(null);
  /** 索引建成的那一刻 +1，触发一次重查拿最终结果。 */
  const [reloadTick, setReloadTick] = useState(0);
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);

  // 关闭重开时重置，避免残留上一次的关键词/筛选。
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setExt(null);
    setDirsOnly(false);
    setResp(null);
    setStatus(null);
    setSelected(null);
    inputRef.current?.focus();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  const trimmed = useMemo(() => query.trim(), [query]);

  // 150ms 防抖查询；索引在 Rust 侧常驻内存，重建中也能边查边补。
  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setLoading(true);
    const t = window.setTimeout(() => {
      invoke<LocalSearchResponse>("local_search", {
        query: trimmed,
        ext,
        dirsOnly,
      })
        .then((r) => {
          if (cancelled) return;
          setResp(r);
          // 查询响应不带 helper_state，保留轮询通道拿到的最新值。
          setStatus((s) => ({
            indexed: r.indexed,
            building: r.building,
            helper_state: s?.helper_state ?? "ok",
          }));
        })
        .catch(() => {
          if (!cancelled) setResp({ hits: [], indexed: 0, building: false });
        })
        .finally(() => {
          if (!cancelled) setLoading(false);
        });
    }, 150);
    return () => {
      cancelled = true;
      window.clearTimeout(t);
    };
  }, [open, trimmed, ext, dirsOnly, reloadTick]);

  // 打开面板时先拉一次索引状态：helper 失败/未授权时 building=false，
  // 不主动拉的话提示条永远拿不到 helper_state。
  useEffect(() => {
    if (!open) return;
    invoke<IndexStatus>("local_search_status")
      .then((s) => setStatus(s))
      .catch(() => {});
  }, [open]);

  // 扫描进度轮询：仅在 building 期间跑，每 500ms 拉一次轻量状态。
  // 之前只在 query/筛选变化时查一次，扫描在后台跑完 UI 都不会再动一下，
  // 于是「已发现 N 项」永远停在 0 —— 这就是统计数字不变的原因。
  useEffect(() => {
    if (!open || !status?.building) return;
    const t = window.setInterval(() => {
      invoke<IndexStatus>("local_search_status")
        .then((s) => {
          setStatus(s);
          // 索引刚建成 → 用完整索引重查一次，之前的结果是残缺的。
          if (!s.building) setReloadTick((n) => n + 1);
        })
        .catch(() => {});
    }, 500);
    return () => window.clearInterval(t);
  }, [open, status?.building]);

  if (!open) return null;

  const hits = resp?.hits ?? [];
  const indexed = status?.indexed ?? 0;
  const building = status?.building ?? false;
  const helperState = status?.helper_state ?? "ok";

  /** 用户点"重新授权/重试"：后端清失败标记并重走提权路线。 */
  const reauthorize = () => {
    invoke("local_search_reauthorize").catch(() => {});
    setStatus((s) => ({ indexed: s?.indexed ?? 0, building: true, helper_state: "ok" }));
  };

  const openHit = (h: LocalSearchHit) => {
    invoke("local_search_open", { path: h.path }).catch(() => {});
    onClose();
  };

  // ↑/↓ 移动选中行，Enter 打开（Explorer 的基本键盘流）。焦点常驻搜索框，
  // 所以同一份处理同时挂在输入框和列表上。
  const onKeyNav = (e: ReactKeyboardEvent) => {
    if (e.key === "Enter" && selected) {
      e.preventDefault();
      openHit(selected);
      return;
    }
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const at = hits.findIndex((h) => h.path === selected?.path);
    const next = e.key === "ArrowDown"
      ? Math.min(hits.length - 1, at + 1)
      : Math.max(0, at - 1);
    const hit = hits[next];
    if (!hit) return;
    setSelected(hit);
    listRef.current
      ?.querySelector<HTMLElement>(`[data-path="${CSS.escape(hit.path)}"]`)
      ?.scrollIntoView({ block: "nearest" });
  };

  const emptyText = loading
    ? "搜索中…"
    : helperState === "unauthorized"
      ? "索引需要一次管理员授权才能建立（秒级，非全盘扫描）"
      : helperState === "failed"
        ? "索引构建失败：未找到可用的索引助手或非 NTFS 卷"
        : building && hits.length === 0
          ? "正在后台扫描本机文件（首次需授权读取 MFT 索引）…"
          : trimmed
            ? "没有匹配的项目"
            : "输入关键词开始查找";

  return createPortal(
    <div className="fixed inset-0 z-[200] flex items-start justify-center p-4 pt-[7vh]">
      <button
        type="button"
        aria-label="关闭"
        onClick={onClose}
        className="absolute inset-0 bg-black/40 backdrop-blur-sm"
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="本地文件"
        className={clsx(
          "relative z-10 w-full max-w-[920px] flex flex-col",
          "rounded-lg shadow-modal overflow-hidden",
          "bg-bg-primary border border-border-primary",
          "h-[70vh]",
        )}
      >
        {/* ① 标题栏 —— Explorer 的窗口标题 */}
        <div className="flex items-center gap-2 h-11 px-3 shrink-0 bg-bg-secondary border-b border-border-primary">
          <FolderOpen className="icon-md text-text-secondary shrink-0" />
          <span className="text-ui font-medium text-text-primary truncate-1">
            {selected ? selected.name : "本地文件"}
          </span>
          <button
            type="button"
            onClick={onClose}
            aria-label="关闭"
            className="icon-btn ml-auto shrink-0"
          >
            <X className="icon-md" />
          </button>
        </div>

        {/* ② 工具栏 —— 搜索框（无边框）+ 类型筛选 */}
        <div className="shrink-0 bg-bg-secondary border-b border-border-primary">
          <div className="flex items-center gap-2 px-3 pt-2">
            {/* 灰底胶囊，全程无描边；焦点只换底色 */}
            <div className="flex-1 flex items-center gap-2 h-8 px-3 rounded-md bg-bg-tertiary focus-within:bg-bg-active transition-colors">
              <Search className="icon-sm text-text-faint shrink-0" />
              <input
                ref={inputRef}
                type="text"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={onKeyNav}
                placeholder="搜索本机文件…"
                className="flex-1 min-w-0 bg-transparent text-ui text-text-primary placeholder:text-text-faint outline-none"
              />
              {query && (
                <button
                  type="button"
                  aria-label="清除"
                  onClick={() => {
                    setQuery("");
                    inputRef.current?.focus();
                  }}
                  className="shrink-0 text-text-faint hover:text-text-primary"
                >
                  <X className="icon-sm" />
                </button>
              )}
              {loading && <Loader2 className="icon-sm text-text-faint shrink-0 animate-spin" />}
            </div>
          </div>
          <div className="flex items-center gap-1 px-3 py-2 flex-wrap">
            {EXT_PRESETS.map((p) => (
              <button
                key={p.ext}
                type="button"
                onClick={() => setExt((v) => (v === p.ext ? null : p.ext))}
                className={clsx(
                  "px-2 py-0.5 rounded-md text-tiny transition-colors",
                  ext === p.ext
                    ? "border border-accent text-accent font-medium"
                    : "text-text-muted hover:bg-bg-tertiary",
                )}
              >
                {p.label}
              </button>
            ))}
            <button
              type="button"
              onClick={() => setDirsOnly((v) => !v)}
              className={clsx(
                "px-2 py-0.5 rounded-md text-tiny transition-colors ml-auto",
                dirsOnly
                  ? "border border-accent text-accent font-medium"
                  : "text-text-muted hover:bg-bg-tertiary",
              )}
            >
              仅文件夹
            </button>
          </div>
          {/* helper 授权/失败提示条：索引建不起来时给原因 + 出口，不再静默空列表 */}
          {(helperState === "unauthorized" || helperState === "failed") && !building && (
            <div className="flex items-center gap-2 px-3 pb-2">
              <div
                className={clsx(
                  "flex-1 flex items-center gap-2 h-8 px-3 rounded-md text-tiny",
                  helperState === "unauthorized"
                    ? "bg-warning-soft text-warning"
                    : "bg-error-soft text-error",
                )}
              >
                <AlertTriangle className="h-3.5 w-3.5 shrink-0" />
                <span className="truncate-1">
                  {helperState === "unauthorized"
                    ? "索引需要一次管理员授权（读取 MFT 索引，秒级完成）"
                    : "索引助手启动失败"}
                </span>
              </div>
              <button
                type="button"
                onClick={reauthorize}
                className="shrink-0 px-3 h-8 rounded-md text-tiny font-medium bg-accent text-white hover:opacity-90 transition-opacity"
              >
                {helperState === "unauthorized" ? "重新授权" : "重试"}
              </button>
            </div>
          )}
        </div>

        {/* ③④ 列表 + 预览 */}
        <div className="flex-1 min-h-0 flex">
          <div className="flex-1 min-w-0 min-h-0 flex flex-col">
            {/* 列头。预览窗格打开时收起「类型/大小」两列——照 Explorer 的
                做法：窗格吃掉宽度后，固定列先把文件名挤没了。 */}
            <div
              className={clsx(
                "shrink-0 grid gap-3 px-3 py-1.5 text-tiny text-text-secondary",
                "bg-bg-secondary border-b border-border-primary",
                selected
                  ? "grid-cols-[minmax(0,1fr)_92px]"
                  : "grid-cols-[minmax(0,1fr)_92px_120px_72px]",
              )}
            >
              <span>名称</span>
              <span>修改日期</span>
              {!selected && <span>类型</span>}
              {!selected && <span className="text-right">大小</span>}
            </div>
            <div
              ref={listRef}
              tabIndex={-1}
              onKeyDown={onKeyNav}
              className="flex-1 min-h-0 overflow-y-auto py-1 outline-none"
            >
              {hits.length === 0 ? (
                <p className="text-meta text-text-muted text-center py-10">{emptyText}</p>
              ) : (
                hits.map((h) => (
                  <button
                    key={h.path}
                    data-path={h.path}
                    type="button"
                    onClick={() => setSelected(h)}
                    onDoubleClick={() => openHit(h)}
                    className={clsx(
                      "w-full text-left px-3 py-1 transition-colors",
                      "grid gap-3 items-center",
                      selected
                        ? "grid-cols-[minmax(0,1fr)_92px]"
                        : "grid-cols-[minmax(0,1fr)_92px_120px_72px]",
                      selected?.path === h.path
                        ? "bg-bg-active"
                        : "hover:bg-bg-tertiary",
                    )}
                    title={h.path}
                  >
                    <span className="flex items-center gap-2 min-w-0">
                      {h.is_dir ? (
                        <Folder className="icon-sm text-text-muted shrink-0" />
                      ) : (
                        <File className="icon-sm text-text-muted shrink-0" />
                      )}
                      <span className="min-w-0">
                        <span
                          className={clsx(
                            "block truncate-1 text-ui",
                            selected?.path === h.path
                              ? "font-semibold"
                              : "font-medium text-text-primary",
                          )}
                        >
                          {h.name}
                        </span>
                        <span className="block truncate-1 text-tiny text-text-faint">
                          {h.path}
                        </span>
                      </span>
                    </span>
                    <span className="text-tiny text-text-secondary tabular-nums">
                      {formatTime(h.modified_secs)}
                    </span>
                    {!selected && (
                      <>
                        <span className="text-tiny text-text-secondary truncate-1">
                          {typeLabel(h.name, h.is_dir)}
                        </span>
                        <span className="text-tiny text-text-secondary text-right tabular-nums">
                          {h.is_dir ? "" : formatSize(h.size)}
                        </span>
                      </>
                    )}
                  </button>
                ))
              )}
            </div>
          </div>

          {/* 预览窗格（选中后展开） */}
          {selected && (
            <div className="w-[42%] min-w-0 min-h-0 shrink-0 border-l border-border-primary bg-bg-secondary">
              <PreviewPane hit={selected} />
            </div>
          )}
        </div>

        {/* ④ 状态栏 —— 左：扫描进度，右：项目数 */}
        <div className="shrink-0 flex items-center gap-2 h-7 px-3 text-tiny text-text-secondary bg-bg-secondary border-t border-border-primary">
          {building ? (
            <>
              <Loader2 className="shrink-0 animate-spin" style={{ width: 11, height: 11 }} />
              <span className="truncate-1">
                正在后台扫描本机文件（已发现 {formatCount(indexed)} 项），结果可能不全…
              </span>
            </>
          ) : (
            <span className="truncate-1">
              已索引 {formatCount(indexed)} 项
            </span>
          )}
          <span className="ml-auto shrink-0 tabular-nums">
            {hits.length > 0 ? `${formatCount(hits.length)} 个项目` : ""}
          </span>
        </div>
      </div>
    </div>,
    document.body,
  );
}
