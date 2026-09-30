import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { clsx } from "clsx";
import {
  FolderSearch, File, Folder, X, Loader2, Clock, Eye,
  FileText, ExternalLink,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

/**
 * 本地快速查找面板（Everything 风格）。
 *
 * 两栏布局：左侧结果列表 + 右侧预览（图片直渲染 / 文本截断读取 /
 * 不支持时说明原因并给「打开」出口）。选中行即预览，双击/回车打开。
 *
 * 层次感：头部搜索行 → 筛选行 → 状态条（索引进度/命中数）→ 结果列表，
 * 四段由上至下视觉重量递减；结果行主次分明（文件名粗体、路径弱化）。
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

function formatSize(bytes: number | null): string {
  if (bytes == null) return "";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

function formatTime(secs: number | null): string {
  if (secs == null) return "";
  const d = new Date(secs * 1000);
  const now = new Date();
  const sameDay = d.toDateString() === now.toDateString();
  if (sameDay) return d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
  return d.toLocaleDateString("zh-CN", { month: "numeric", day: "numeric" });
}

/** ── 右侧预览 ─────────────────────────────────────────────── */

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
        <p className="text-[12px]">选中左侧结果进行预览</p>
      </div>
    );
  }

  return (
    <div className="flex flex-col min-h-0 h-full">
      {/* 预览头：文件名 + 打开按钮 */}
      <div className="flex items-center gap-2 px-3 py-2 border-b border-border-subtle shrink-0">
        <span className="flex-1 truncate-1 text-[12.5px] font-medium text-text-primary" title={hit.path}>
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
            <p className="text-[12px]">{hit.is_dir ? "文件夹 · 双击打开" : "无预览"}</p>
          </div>
        ) : preview.kind === "image" ? (
          <div className="flex items-center justify-center p-4">
            <img
              src={preview.data_url}
              alt={hit.name}
              className="max-w-full max-h-full object-contain rounded-lg border border-border-subtle"
            />
          </div>
        ) : preview.kind === "text" ? (
          <div className="p-3">
            <pre className="whitespace-pre-wrap break-all text-[11.5px] leading-relaxed text-text-secondary font-mono">
              {preview.text}
            </pre>
            {preview.truncated && (
              <p className="text-tiny text-text-faint mt-2 pt-2 border-t border-border-subtle">
                内容已截断（超过 256 KB），打开查看完整文件
              </p>
            )}
          </div>
        ) : (
          <div className="flex flex-col items-center justify-center gap-2 py-16 text-text-faint px-6 text-center">
            {preview.kind === "none" && <FileText className="icon-md" />}
            <p className="text-[12px]">
              {preview.kind === "none" ? preview.reason : "无预览"}
            </p>
            {preview.kind === "none" && (
              <button
                type="button"
                className="mt-1 px-3 py-1 rounded-md text-[12px] border border-border-subtle text-text-secondary hover:bg-bg-tertiary transition-colors"
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
  const [loading, setLoading] = useState(false);
  const [selected, setSelected] = useState<LocalSearchHit | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // 关闭重开时重置，避免残留上一次的关键词/筛选。
  useEffect(() => {
    if (!open) return;
    setQuery("");
    setExt(null);
    setDirsOnly(false);
    setResp(null);
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
          if (!cancelled) setResp(r);
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
  }, [open, trimmed, ext, dirsOnly]);

  if (!open) return null;

  const hits = resp?.hits ?? [];
  const building = resp?.building ?? false;

  const openHit = (h: LocalSearchHit) => {
    invoke("local_search_open", { path: h.path }).catch(() => {});
    onClose();
  };

  return createPortal(
    <div className="fixed inset-0 z-[200] flex items-start justify-center p-4 pt-[10vh]">
      <button
        type="button"
        aria-label="关闭"
        onClick={onClose}
        className="absolute inset-0 bg-black/40 backdrop-blur-sm"
      />
      <div
        role="dialog"
        aria-modal="true"
        aria-label="本地查找"
        className={clsx(
          "relative z-10 w-full max-w-[860px] flex flex-col",
          "rounded-xl border shadow-2xl overflow-hidden",
          "bg-bg-secondary border-border-subtle",
          "h-[64vh]",
        )}
      >
        {/* ① 头部搜索行 —— 最重视觉层 */}
        <div className="flex items-center gap-3 px-4 py-3 border-b border-border-subtle shrink-0 bg-bg-tertiary/40">
          <FolderSearch className="icon-md text-text-muted shrink-0" />
          <input
            ref={inputRef}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="查找本机文件…（模糊匹配，支持文件名/路径）"
            className="flex-1 bg-transparent text-ui text-text-primary placeholder:text-text-faint outline-none"
          />
          {loading && <Loader2 className="icon-sm text-text-faint shrink-0 animate-spin" />}
          <button type="button" onClick={onClose} aria-label="关闭" className="icon-btn shrink-0">
            <X className="icon-md" />
          </button>
        </div>

        {/* ② 筛选行 —— 次级控制层 */}
        <div className="flex items-center gap-1.5 px-4 py-2 border-b border-border-subtle shrink-0 flex-wrap">
          {EXT_PRESETS.map((p) => (
            <button
              key={p.ext}
              type="button"
              onClick={() => setExt((v) => (v === p.ext ? null : p.ext))}
              className={clsx(
                "px-2 py-0.5 rounded-md text-[11px] border transition-colors",
                ext === p.ext
                  ? "bg-accent-soft border-accent text-accent font-medium"
                  : "border-border-subtle text-text-muted hover:bg-bg-tertiary",
              )}
            >
              {p.label}
            </button>
          ))}
          <button
            type="button"
            onClick={() => setDirsOnly((v) => !v)}
            className={clsx(
              "px-2 py-0.5 rounded-md text-[11px] border transition-colors ml-auto",
              dirsOnly
                ? "bg-accent-soft border-accent text-accent font-medium"
                : "border-border-subtle text-text-muted hover:bg-bg-tertiary",
            )}
          >
            仅文件夹
          </button>
        </div>

        {/* ③ 状态条 —— 一行小字交代索引与命中 */}
        <div className="flex items-center gap-2 px-4 py-1.5 shrink-0 text-tiny text-text-faint border-b border-border-subtle/60">
          {building ? (
            <>
              <Loader2 className="icon-sm animate-spin" style={{ width: 11, height: 11 }} />
              <span>正在后台索引本机文件（已发现 {resp?.indexed ?? 0} 项），结果可能不全…</span>
            </>
          ) : (
            <span>索引 {resp?.indexed ?? 0} 项 · 命中 {hits.length} 条</span>
          )}
          {selected && (
            <span className="ml-auto truncate-1 max-w-[40%]" title={selected.path}>
              {selected.path}
            </span>
          )}
        </div>

        {/* ④ 两栏内容区：左列表 + 右预览 */}
        <div className="flex-1 min-h-0 flex">
          {/* 左：结果列表 */}
          <div
            className={clsx(
              "min-h-0 overflow-y-auto p-1.5 shrink-0",
              selected ? "w-[55%] border-r border-border-subtle" : "w-full",
            )}
          >
            {hits.length === 0 ? (
              <p className="text-[12.5px] text-text-muted text-center py-10">
                {loading
                  ? "搜索中…"
                  : building && hits.length === 0
                    ? "正在索引本机文件（首次较慢，之后瞬时）…"
                    : trimmed
                      ? "没有匹配的文件"
                      : "输入关键词开始查找"}
              </p>
            ) : (
              hits.map((h) => (
                <button
                  key={h.path}
                  type="button"
                  onClick={() => setSelected(h)}
                  onDoubleClick={() => openHit(h)}
                  className={clsx(
                    "w-full text-left px-3 py-1.5 rounded-lg transition-colors",
                    selected?.path === h.path
                      ? "bg-accent-soft"
                      : "hover:bg-bg-tertiary",
                  )}
                  title={h.path}
                >
                  <div className="flex items-center gap-2">
                    {h.is_dir ? (
                      <Folder className="icon-sm text-text-muted shrink-0" />
                    ) : (
                      <File className="icon-sm text-text-muted shrink-0" />
                    )}
                    <span
                      className={clsx(
                        "flex-1 truncate-1 text-[13px]",
                        selected?.path === h.path
                          ? "text-accent font-semibold"
                          : "text-text-primary font-medium",
                      )}
                    >
                      {h.name}
                    </span>
                    {h.size != null && (
                      <span className="text-tiny text-text-faint shrink-0 tabular-nums">
                        {formatSize(h.size)}
                      </span>
                    )}
                    {h.modified_secs != null && (
                      <span className="text-tiny text-text-faint shrink-0 flex items-center gap-1 w-[52px] justify-end">
                        <Clock size={10} />
                        {formatTime(h.modified_secs)}
                      </span>
                    )}
                  </div>
                  <p
                    className={clsx(
                      "text-[11px] truncate-1 mt-0.5 ml-5",
                      selected?.path === h.path ? "text-accent/70" : "text-text-faint",
                    )}
                  >
                    {h.path}
                  </p>
                </button>
              ))
            )}
          </div>

          {/* 右：预览栏（选中后展开） */}
          {selected && (
            <div className="flex-1 min-w-0 min-h-0 bg-bg-primary/40">
              <PreviewPane hit={selected} />
            </div>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}
