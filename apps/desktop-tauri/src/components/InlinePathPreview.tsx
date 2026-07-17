import { useEffect, useState } from "react";
import { X, ExternalLink, Copy, Check, Loader2, AlertTriangle, FileText, FileCode, FileJson, FileImage, FileType2, Folder } from "lucide-react";
import { clsx } from "clsx";
import { openExternalUrl } from "../desktopBridge";
import { tryReadArtifact, tryRenderOfficePreview } from "../utils/artifactReader";
import { classifyArtifactKind } from "../utils/artifacts";

interface InlinePathPreviewProps {
  /** Raw path the user clicked (may be relative — backend will anchor). */
  path: string;
  /** Workspace root to anchor relative paths against. */
  workspaceRoot?: string | null;
  /** Called when the user dismisses the popover. */
  onClose: () => void;
  /** Anchor rect for positioning (default: bottom-right of the trigger). */
  anchor?: { top: number; left: number };
}

type PreviewState =
  | { status: "loading" }
  | { status: "text"; content: string; resolvedPath: string }
  | { status: "binary"; size: number; resolvedPath: string }
  | { status: "empty"; resolvedPath: string }
  | { status: "error"; message: string }
  | { status: "office"; htmlPath: string; htmlContent: string; fileName: string; resolvedPath: string };

const OFFICE_EXTS = new Set(["docx", "doc", "xlsx", "xls", "pptx", "ppt"]);

// 2026-07-17: extension→kind mapping used to be defined here with its
// own (smaller) extension set, drifted from the App.tsx duplicate and
// from the canonical one in utils/artifacts.ts. Now uses the shared
// `classifyArtifactKind` so the same file renders the same icon /
// lands in the same preview branch regardless of which surface the
// user clicked it from.
const classify = classifyArtifactKind;

function FileIcon({ name, size = 12 }: { name: string; size?: number }) {
  const k = classify(name);
  if (k === "code") return <FileCode size={size} className="text-info shrink-0" />;
  if (k === "doc") return <FileText size={size} className="text-accent shrink-0" />;
  if (k === "data") return <FileJson size={size} className="text-warning shrink-0" />;
  if (k === "image") return <FileImage size={size} className="text-success shrink-0" />;
  return <FileType2 size={size} className="text-text-muted shrink-0" />;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

/** Translate raw backend error strings into user-friendly Chinese.
 * Mirrors the same translation table in PreviewPanel so the inline
 * popover and the right-side panel give consistent error copy. */
function translatePreviewError(raw: string): string {
  if (/cannot resolve path|cannot find the path|系统找不到指定的文件|os error 2/i.test(raw)) {
    return "文件不存在或已移动。可能原因：被删除、路径拼写错误、引用了 traceback / 示例中的路径。";
  }
  if (/outside the allowed artifact roots/i.test(raw)) {
    return "该路径不在允许的产物根目录内（出于安全考虑被拒绝）。";
  }
  if (/file too large/i.test(raw)) {
    return "文件过大，仅支持预览 ≤ 2 MiB 的文本文件。";
  }
  return raw;
}

export function InlinePathPreview({ path, workspaceRoot, onClose, anchor }: InlinePathPreviewProps) {
  const [state, setState] = useState<PreviewState>({ status: "loading" });
  const [copied, setCopied] = useState(false);

  // ESC closes
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  useEffect(() => {
    let cancelled = false;
    const name = path.split(/[\\/]/).pop() || path;
    const ext = name.split(".").pop()?.toLowerCase() ?? "";

    // Office files: use renderOfficePreview with root-retry fallback.
    if (OFFICE_EXTS.has(ext)) {
      tryRenderOfficePreview(path, workspaceRoot).then((res) => {
        if (cancelled) return;
        if ("error" in res) {
          setState({ status: "error", message: res.error });
        } else {
          setState({
            status: "office",
            htmlPath: res.htmlPath,
            htmlContent: res.htmlContent,
            fileName: res.fileName,
            resolvedPath: res.resolvedPath,
          });
        }
      });
      return () => { cancelled = true; };
    }

    // Text / binary / empty read with root-retry fallback so paths
    // outside the user's configured workspaceRoot still resolve via
    // their drive root or an ancestor directory.
    tryReadArtifact(path, workspaceRoot).then((res) => {
      if (cancelled) return;
      if (res.kind === "text") {
        setState({ status: "text", content: res.content, resolvedPath: res.resolvedPath });
      } else if (res.kind === "binary") {
        setState({ status: "binary", size: res.size, resolvedPath: res.resolvedPath });
      } else if (res.kind === "empty") {
        setState({ status: "empty", resolvedPath: res.resolvedPath });
      } else {
        setState({ status: "error", message: res.message });
      }
    });
    return () => {
      cancelled = true;
    };
  }, [path, workspaceRoot]);

  const name = path.split(/[\\/]/).pop() || path;
  const isAbsolute = /^(?:[A-Za-z]:[\\/])|(?:[\/])/.test(path);

  // Position: anchored to bottom-right of `anchor`, or just right side of the screen.
  // Keep on-screen by clamping at render time below.
  const style: React.CSSProperties = anchor
    ? {
        top: Math.max(16, Math.min(anchor.top, window.innerHeight - 360)),
        left: Math.max(16, Math.min(anchor.left, window.innerWidth - 560)),
      }
    : { top: 64, right: 24 };

  return (
    <>
      {/* Backdrop with click-outside */}
      <div
        className="fixed inset-0 z-40 bg-black/0 animate-[h-modal-enter_120ms_ease-out]"
        onClick={onClose}
      />
      <div
        className={clsx(
          "fixed z-50 w-[min(560px,calc(100vw-32px))] max-h-[70vh] flex flex-col",
          "rounded-xl bg-bg-elevated overflow-hidden animate-[h-modal-enter_160ms_ease-out]",
        )}
        style={style}
        role="dialog"
        aria-modal="true"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="px-3 py-2 flex items-center gap-2 bg-bg-tertiary shrink-0">
          <FileIcon name={name} size={13} />
          <span className="text-[12.5px] text-text-primary font-medium truncate flex-1" title={path}>
            {name}
          </span>
          {!isAbsolute && (
            <span className="text-[9.5px] px-1.5 py-0.5 rounded bg-warning/15 text-warning font-mono shrink-0">
              相对
            </span>
          )}
          {state.status === "text" && (
            <button
              onClick={async () => {
                await navigator.clipboard.writeText(state.content);
                setCopied(true);
                setTimeout(() => setCopied(false), 1500);
              }}
              className="p-1 rounded hover:bg-bg-hover text-text-muted hover:text-text-primary transition-colors"
              title="复制内容"
            >
              {copied ? <Check size={11} /> : <Copy size={11} />}
            </button>
          )}
          {state.status === "text" && (
            <button
              onClick={() => {
                // Best-effort: try to open in shell. We re-derive the absolute
                // path from the resolved result so the OS handler can find it.
                const target = state.resolvedPath || path;
                import("../desktopBridge").then((b) => b.openExternalUrl(`file:///${target.replace(/\\/g, "/")}`));
              }}
              className="p-1 rounded hover:bg-bg-hover text-text-muted hover:text-text-primary transition-colors"
              title="在系统资源管理器中打开"
            >
              <ExternalLink size={11} />
            </button>
          )}
          <button
            onClick={onClose}
            className="p-1 rounded hover:bg-bg-hover text-text-muted hover:text-text-primary transition-colors"
            title="关闭 (ESC)"
           aria-label="关闭 (ESC)">
            <X size={11} />
          </button>
        </div>

        {/* Resolved-path strip — surfaces the canonical absolute path
            returned by the backend when the LLM only said "tmp/xx.md". */}
        {state.status !== "loading" && state.status !== "error" && state.resolvedPath !== path && (
          <div
            className="flex items-center gap-1.5 px-3 py-1.5 text-[12px] font-mono truncate bg-bg-card/40 border-b border-border-subtle cursor-pointer"
            onClick={() => {
              const dir = state.resolvedPath.replace(/^\\\\\?\\/, "").replace(/[/\\][^/\\]+$/, "");
              openExternalUrl(`file:///${dir.replace(/\\/g, "/")}`);
            }}
          >
            <Folder size={12} className="shrink-0 text-info" />
            <span className="text-info underline underline-offset-2 decoration-info/40 truncate">{state.resolvedPath.replace(/^\\\\\?\\/, "")}</span>
          </div>
        )}

        {/* Body */}
        <div className="flex-1 overflow-auto p-3 bg-bg-primary min-h-0">
          {state.status === "loading" && (
            <div className="flex items-center gap-2 text-[11.5px] text-text-muted">
              <Loader2 size={12} className="animate-spin" />
              正在读取 {path}…
            </div>
          )}
          {state.status === "error" && (
            <div className="flex items-start gap-2 text-[11.5px] text-error">
              <AlertTriangle size={12} className="shrink-0 mt-0.5" />
              <span className="break-all">{translatePreviewError(state.message)}</span>
            </div>
          )}
          {state.status === "binary" && (
            <div className="text-[11.5px] text-text-muted">
              // 二进制文件（{formatBytes(state.size)}），不支持文本预览
            </div>
          )}
          {state.status === "empty" && (
            <div className="text-[11.5px] text-text-faint">// 空文件</div>
          )}
          {state.status === "text" && (
            <pre className="font-mono text-[11.5px] leading-relaxed text-text-secondary whitespace-pre-wrap break-all">
{state.content}
            </pre>
          )}
          {state.status === "office" && (
            <div className="w-full rounded-md overflow-hidden border border-border-subtle" style={{ background: "#fff", minHeight: "300px" }}>
              <iframe
                srcDoc={state.htmlContent}
                className="w-full border-0"
                title={state.fileName}
                sandbox="allow-same-origin"
                style={{ height: "400px" }}
              />
            </div>
          )}
        </div>
      </div>
    </>
  );
}
