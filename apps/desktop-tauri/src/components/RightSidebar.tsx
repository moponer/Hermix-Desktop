import { useEffect, useMemo, useRef, useState, useCallback } from "react";
import { createPortal } from "react-dom";
import {
  X, Search, FileText, FileCode, FileJson,
  FileImage, FileType2, FolderOpen,
  Loader2, AlertTriangle, Monitor, Globe,
  ChevronRight, ChevronDown, File,
  Terminal, Copy, Eye,
} from "lucide-react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { CodeView } from "./CodeView";
import { SpreadsheetView } from "./SpreadsheetView";
import { FileTypeIcon } from "./FileTypeIcon";
import { clsx } from "clsx";
import type { Artifact, ArtifactKind } from "../utils/artifacts";
import { openExternalUrl } from "../desktopBridge";
import {
  tryReadArtifact,
  tryReadArtifactBytes,
  tryRenderOfficePreview,
} from "../utils/artifactReader";
import { parseByExtension, type PreviewModel } from "../lib/fileParser";

// ── Tab types ──────────────────────────────────────────
type RightTab = "computer" | "products" | "browser";

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
}

const FILTERS: Array<{ key: "all" | ArtifactKind; label: string; icon: React.ElementType }> = [
  { key: "all", label: "全部", icon: FolderOpen },
  { key: "code", label: "代码", icon: FileCode },
  { key: "doc", label: "文档", icon: FileText },
  { key: "data", label: "数据", icon: FileJson },
  { key: "image", label: "图片", icon: FileImage },
  { key: "office", label: "Office", icon: FileType2 },
];


type PreviewState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "text"; content: string }
  | { status: "binary"; size: number }
  | { status: "empty" }
  | { status: "error"; message: string }
  | { status: "office"; htmlContent: string; fileName: string }
  | { status: "spreadsheet"; model: PreviewModel; fileName: string }
  // New: image preview — base64 data URL + dimensions.  We embed
  // via `data:` URL so the iframe-less modal has no CORS or CSP
  // surprises, and so the image renders even when the workspace
  // root is on a network drive the Tauri CSP wouldn't expose.
  | { status: "image"; dataUrl: string; mime: string; size: number; width?: number; height?: number }
  // New: markdown preview — pre-rendered HTML from react-markdown
  // (done here to keep CodeView's react-markdown import surface
  // contained).  `inlineCss` is a small theme-aware stylesheet
  // we splice into the body so the markdown adopts the modal's
  // text colors without a dedicated stylesheet file.
  | { status: "markdown"; content: string; fileName: string }
  // New: PDF preview — blob URL from raw bytes, rendered in
  // an <embed> tag using the browser's native PDF viewer.
  // `fileName` is used for the embed's `title` attribute.
  | { status: "pdf"; blobUrl: string; fileName: string };

/** File extensions the front-end parses itself. We do not
 * shell out to `officecli` for these — see `lib/fileParser.ts`. */
const FRONTEND_PARSE_EXTS = new Set([
  "xlsx", "xls", "xlsm",
  "csv", "tsv",
]);

function needsFrontendParse(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return FRONTEND_PARSE_EXTS.has(ext);
}

/** Sum the rows across every sheet in a parsed PreviewModel.
 * Pulled out of JSX so we can also re-use it in the fullscreen
 * status bar without repeating the discriminated-union check. */
function totalRows(model: PreviewModel): number {
  if (model.kind === "spreadsheet") return model.sheets.reduce((s, sh) => s + sh.rowCount, 0);
  if (model.kind === "csv") return model.rowCount;
  return 0;
}

function modelLabel(model: PreviewModel): string {
  if (model.kind === "spreadsheet") return `${model.sheets.length} 个工作表`;
  if (model.kind === "csv") return "CSV";
  return "Text";
}

/* ─────────────────────────────────────────────────────────────────────
   Language label map + size formatter — shared by the new header
   ───────────────────────────────────────────────────────────────────── */
const LANG_LABELS: Record<string, string> = {
  typescript: "TypeScript",
  tsx: "TSX",
  javascript: "JavaScript",
  jsx: "JSX",
  rust: "Rust",
  python: "Python 3",
  go: "Go",
  java: "Java",
  kotlin: "Kotlin",
  c: "C",
  cpp: "C++",
  objc: "Objective-C",
  ruby: "Ruby",
  php: "PHP",
  bash: "Bash",
  shell: "Shell",
  powershell: "PowerShell",
  css: "CSS",
  scss: "SCSS",
  less: "Less",
  html: "HTML",
  json: "JSON",
  yaml: "YAML",
  xml: "XML",
  ini: "INI",
  sql: "SQL",
  lua: "Lua",
  swift: "Swift",
  markdown: "Markdown",
  text: "Text",
};

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}




/* Image extensions routed through the `image` preview path.
   The `mime` map is consulted to build a data: URL — we don't
   trust the OS to report MIME correctly for every container
   (.webp on Windows is still hit-or-miss). */
const IMAGE_EXT_MIME: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
  bmp: "image/bmp",
  ico: "image/x-icon",
  tiff: "image/tiff",
  tif: "image/tiff",
};
const IMAGE_BYTE_LIMIT = 8 * 1024 * 1024; // 8 MiB; above this we fall through to a friendly error

function isImageExt(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return ext in IMAGE_EXT_MIME;
}

function isMarkdownExt(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return ext === "md" || ext === "mdx" || ext === "markdown";
}

function isPdfExt(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return ext === "pdf";
}

function detectLanguage(name: string): string | null {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    ts: "typescript", tsx: "tsx", js: "javascript", jsx: "jsx", mjs: "javascript",
    rs: "rust", py: "python", go: "go", java: "java", kt: "kotlin",
    c: "c", cpp: "cpp", cxx: "cpp", h: "c", hpp: "cpp",
    rb: "ruby", php: "php", sh: "bash", bash: "bash",
    css: "css", html: "html", vue: "html",
    sql: "sql", json: "json", yaml: "yaml", yml: "yaml", toml: "ini", xml: "xml",
    md: "markdown", txt: "text", csv: "text",
  };
  // Treat markdown as plain text (no syntax highlighting effects)
  const lang = map[ext] ?? null;
  if (lang === "markdown") return null;
  return lang;
}

export function PreviewPanel({
  artifact,
  workspaceRoot,
  onBack,
}: {
  artifact: Artifact | null;
  workspaceRoot?: string | null;
  onBack: () => void;
}) {
  const [previewState, setPreviewState] = useState<PreviewState>({ status: "idle" });
  const [resolvedPath, setResolvedPath] = useState<string | null>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [activeLine, setActiveLine] = useState(0);
  const [cursor, setCursor] = useState<{ line: number; col: number }>({ line: 1, col: 1 });

  // Cleanup previous blob URL when preview state changes, so we
  // don't leak memory when switching between PDFs (or away from
  // a PDF).  blob: URLs are not auto-reclaimed by the browser
  // when the element is removed — URL.revokeObjectURL is the
  // only reliable cleanup path.
  const prevBlobRef = useRef<string | null>(null);
  const stateRef = useRef(previewState);
  stateRef.current = previewState;
  useEffect(() => {
    const st = stateRef.current;
    if (st.status === "pdf" && prevBlobRef.current !== st.blobUrl) {
      if (prevBlobRef.current) URL.revokeObjectURL(prevBlobRef.current);
      prevBlobRef.current = st.blobUrl;
    }
    return () => {
      // Component unmount — revoke the live blob URL
      if (prevBlobRef.current) {
        URL.revokeObjectURL(prevBlobRef.current);
        prevBlobRef.current = null;
      }
    };
  }, [previewState]);

  // Fetch file content when artifact changes.
  //
  // This MUST live in an effect, not in the render body — calling
  // setState during render triggers:
  //   "Cannot update a component (`App`) while rendering a
  //    different component (`RightSidebar`)"
  // because the state update propagates to App's onSelectArtifact
  // callback and ends up re-rendering App mid-render.
  //
  // The cancellation flag prevents a stale async resolution from
  // overwriting the state of a newer artifact after the user has
  // already switched files.
  useEffect(() => {
    if (!artifact?.path) {
      setPreviewState({ status: "idle" });
      setResolvedPath(null);
      return;
    }

    let cancelled = false;
    setPreviewState({ status: "loading" });

    // Image preview — fetched as raw bytes and embedded as a
    // data: URL.  We can't just point an <img src=...> at the
    // file path because the Tauri CSP blocks file:// in the
    // webview, and even when allowed the path may include
    // spaces / CJK that need URL-encoding. data: URL sidesteps
    // both problems and works fully offline.
    if (isImageExt(artifact.name)) {
      tryReadArtifactBytes(artifact.path, workspaceRoot).then((res) => {
        if (cancelled) return;
        if (res.kind === "error") {
          setPreviewState({ status: "error", message: res.message });
          return;
        }
        if (res.size > IMAGE_BYTE_LIMIT) {
          setPreviewState({
            status: "error",
            message: `图片过大（${formatBytes(res.size)}），预览上限 ${formatBytes(IMAGE_BYTE_LIMIT)}。`,
          });
          return;
        }
        const ext = artifact.name.split(".").pop()?.toLowerCase() ?? "";
        const mime = IMAGE_EXT_MIME[ext] ?? "application/octet-stream";
        // Build base64 from raw bytes — `String.fromCharCode` on
        // chunks is the well-known fast path that handles large
        // buffers without stack overflow.
        let bin = "";
        const chunk = 0x8000;
        for (let i = 0; i < res.bytes.length; i += chunk) {
          bin += String.fromCharCode.apply(
            null,
            Array.from(res.bytes.subarray(i, i + chunk)) as unknown as number[],
          );
        }
        const dataUrl = `data:${mime};base64,${btoa(bin)}`;
        setResolvedPath(res.resolvedPath);
        setPreviewState({ status: "image", dataUrl, mime, size: res.size });
      });
    } else if (isPdfExt(artifact.name)) {
      // PDF — read as raw bytes, create a blob URL, render in
      // an <embed> tag.  Browsers have native PDF viewers so we
      // don't need pdfjs-dist.  Limit to 50 MiB (most real PDFs
      // are under 10 MiB but some scans go higher).
      tryReadArtifactBytes(artifact.path, workspaceRoot).then((res) => {
        if (cancelled) return;
        if (res.kind === "error") {
          setPreviewState({ status: "error", message: res.message });
          return;
        }
        const LIMIT = 50 * 1024 * 1024; // 50 MiB
        if (res.size > LIMIT) {
          setPreviewState({
            status: "error",
            message: `PDF 过大（${formatBytes(res.size)}），预览上限 ${formatBytes(LIMIT)}。`,
          });
          return;
        }
        const blobUrl = URL.createObjectURL(new Blob([res.bytes], { type: "application/pdf" }));
        setResolvedPath(res.resolvedPath);
        setPreviewState({ status: "pdf", blobUrl, fileName: artifact.name });
      });
    } else if (isMarkdownExt(artifact.name)) {
      // Markdown — read as text, hand off to react-markdown.
      tryReadArtifact(artifact.path, workspaceRoot).then((res) => {
        if (cancelled) return;
        if (res.kind === "text") {
          setResolvedPath(res.resolvedPath);
          setPreviewState({ status: "markdown", content: res.content, fileName: artifact.name });
        } else if (res.kind === "empty") {
          setResolvedPath(res.resolvedPath);
          setPreviewState({ status: "empty" });
        } else if (res.kind === "binary") {
          setPreviewState({ status: "binary", size: res.size });
        } else {
          setPreviewState({ status: "error", message: res.message });
        }
      });
    } else if (artifact.kind === "office" && needsFrontendParse(artifact.name)) {
      tryReadArtifactBytes(artifact.path, workspaceRoot).then((res) => {
        if (cancelled) return;
        if (res.kind === "error") {
          setPreviewState({ status: "error", message: res.message });
          return;
        }
        const model = parseByExtension(artifact.name, res.bytes);
        if (model.kind === "error") {
          setPreviewState({ status: "error", message: model.message });
          return;
        }
        if (model.kind === "binary") {
          setPreviewState({ status: "binary", size: model.size });
          return;
        }
        setResolvedPath(res.resolvedPath);
        setPreviewState({ status: "spreadsheet", model, fileName: artifact.name });
      });
    } else if (artifact.kind === "office") {
      tryRenderOfficePreview(artifact.path, workspaceRoot).then((res) => {
        if (cancelled) return;
        if ("error" in res) {
          let message: string;
          if (/unsupported file type/i.test(res.error)) {
            message = "不支持的 Office 文件格式。";
          } else if (/officecli binary not found/i.test(res.error)) {
            message = "OfficeCLI 渲染引擎未找到，请检查应用安装完整性。";
          } else {
            message = res.error;
          }
          setPreviewState({ status: "error", message });
        } else {
          setPreviewState({ status: "office", htmlContent: res.htmlContent, fileName: res.fileName });
        }
      });
    } else {
      tryReadArtifact(artifact.path, workspaceRoot).then((res) => {
        if (cancelled) return;
        if (res.kind === "text") {
          setResolvedPath(res.resolvedPath);
          setPreviewState({ status: "text", content: res.content });
        } else if (res.kind === "binary") {
          setResolvedPath(res.resolvedPath);
          setPreviewState({ status: "binary", size: res.size });
        } else if (res.kind === "empty") {
          setResolvedPath(res.resolvedPath);
          setPreviewState({ status: "empty" });
        } else {
          let message: string;
          if (/cannot resolve path|cannot find the path|系统找不到指定的文件|os error 2/i.test(res.message)) {
            message = "文件不存在或已移动。";
          } else if (/outside the allowed artifact roots/i.test(res.message)) {
            message = "该路径不在允许的产物根目录内（出于安全考虑被拒绝）。";
          } else if (/file too large/i.test(res.message)) {
            message = "文件过大，仅支持预览 ≤ 2 MiB 的文本文件。";
          } else {
            message = res.message;
          }
          setPreviewState({ status: "error", message });
        }
      });
    }

    return () => { cancelled = true; };
  }, [artifact?.path, artifact?.kind, workspaceRoot]);

  const codeLang = artifact ? detectLanguage(artifact.name) : null;


  const openContainingDir = () => {
    if (!artifact) return;
    const target = (resolvedPath ?? artifact.path).replace(/^\\\\\?\\/, "");
    const dir = target.replace(/[/\\][^/\\]+$/, "");
    openExternalUrl(dir);
  };

  /* ── Fullscreen: capture keydown at document level (React's onKeyDown
        on a non-focusable <div> doesn't fire for Escape). Cleanup on
        unmount so the listener doesn't leak when leaving the panel. */
  useEffect(() => {
    if (!fullscreen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setFullscreen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [fullscreen]);

  // Pre-compute header data once for both panel + fullscreen overlay
  const lang = artifact ? detectLanguage(artifact.name) : null;
  // For recognised code/document languages we show the full label
  // (e.g. "TypeScript", "Python 3", "Markdown").  For everything
  // else (PDF, xlsx, png, docx…) `detectLanguage` returns null and
  // the old code fell through to the misleading "Plain text".
  // Instead we extract the extension and present it as a preview
  // label — the FileTypeIcon brand block already conveys the type
  // visually, but a text chip helps users who scan by reading.
  const langLabel = lang
    ? (LANG_LABELS[lang] ?? lang)
    : (() => {
        const ext = artifact?.name.split(".").pop()?.toLowerCase() ?? "";
        const MAP: Record<string, string> = {
          pdf: "PDF",
          docx: "Word", doc: "Word", odt: "Word", rtf: "RTF",
          xlsx: "Excel", xls: "Excel", ods: "Spreadsheet",
          csv: "CSV", tsv: "TSV",
          pptx: "PowerPoint", ppt: "PowerPoint", odp: "Presentation",
          png: "PNG", jpg: "JPEG", jpeg: "JPEG", gif: "GIF",
          webp: "WebP", bmp: "BMP", ico: "Icon", svg: "SVG",
          mp3: "Audio", wav: "Audio", mp4: "Video", mov: "Video",
          zip: "Archive", rar: "Archive", "7z": "Archive", tar: "Archive",
          exe: "Executable", dll: "DLL",
          lock: "Lockfile",
        };
        return (MAP[ext] ?? ext.toUpperCase()) || "预览";
      })();
  const displayPath = artifact ? (resolvedPath ?? artifact.path) : "";
  const cleanPath = displayPath.replace(/^\\\\\?\\/, "");
  const textContent = previewState.status === "text" ? previewState.content : "";

  // Reset active line + cursor when switching files
  useEffect(() => {
    setActiveLine(0);
    setCursor({ line: 1, col: 1 });
  }, [artifact?.path]);

  /* ── Shared header JSX — used by both the panel and the fullscreen
        portal overlay so they stay visually identical.

        v4 layout: just two visual rows.
        ── Row 1 (top):   icon · filename + lang chip · close button
        ── Row 2 (meta):  full path (one line, truncates) · action group
        No more 3-row jammed header, no redundant breadcrumb / line-count
        / size chips fighting for the same horizontal slot. ── */
  const renderHeader = (isFs: boolean) => {
    if (!artifact) return null;
    return (
      <div className={clsx("preview-head", isFs && "is-fullscreen")}>
        {/* ── Row 1: icon · name · lang · close ─────────────────── */}
        <div className="preview-head-row1">
          {/* Use the rich OS-style branded file icon (PDF/DOCX/XLSX
              etc.) keyed off the *filename* extension.  Falls back
              to the legacy colored tile if no extension match. */}
          {(() => {
            // Modal header icon — sized to dominate the row
            // (主人 2026-06-22: "head 上面的可以大一点").
            // 40 / 34 px reads as a real file tab header, not a
            // little badge tucked next to a filename.  At 40px
            // the dog-ear, label and brand color all stay crisp.
            const sz = isFs ? 44 : 40;
            return (
              <div
                className="file-icon-tile file-icon-tile--branded"
                style={{
                  width: sz,
                  height: sz,
                  background: "transparent",
                  boxShadow: "none",
                }}
                title={artifact.name}
              >
                <FileTypeIcon name={artifact.name} size={sz} style={{ display: "block" }} />
              </div>
            );
          })()}

          <span className="preview-filename">{artifact.name}</span>
          <span className="preview-lang">
            <span className="preview-lang-dot" />
            {langLabel}
          </span>

          <div className="preview-head-spacer" />

          <button
            type="button"
            onClick={onBack}
            title="关闭预览"
            className="head-action is-danger"
          >
            <X size={14} />
          </button>
        </div>

        {/* ── Row 2: path · action group ────────────────────────── */}
        <div className="preview-head-row2">
          <button
            type="button"
            className="preview-path"
            title={`打开所在目录：${cleanPath}`}
            onClick={openContainingDir}
          >
            {cleanPath}
          </button>

          <div className="preview-head-spacer" />

          <div className="preview-head-actions">
            {/* Note (主人 2026-06-22): 移除全屏切换 — modal 框体大小已经
                固定且合适，主人明确说"不要有最大化功能，就这么大就
                可以了"。如果以后要在 modal 内部再加 maximize 入口，
                重新引入 `Maximize2` import 即可。 */}
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className="flex flex-col h-full min-h-0 overflow-hidden preview-pane">
      {renderHeader(false)}

      {/* Body — min-h-0 enables scrollbar for long content.
          Background uses --color-bg-code (auto light/dark) so the
          editor surface stays in sync with the header. The body
          has its own gradient overlay (defined in code-view.css)
          that gives the top edge a subtle shadow so the header
          "lifts" off the code surface. */}
      <div
        className="flex-1 flex flex-col min-h-0 preview-body"
        style={{ background: "var(--color-bg-code)" }}
      >
        <div className="preview-pane-inner flex-1 overflow-auto min-h-0">
          {previewState.status === "loading" && (
            <div className="flex items-center justify-center h-full gap-2 text-[12px] text-text-muted preview-loading">
              <Loader2 size={13} className="animate-spin" />
              正在读取文件…
            </div>
          )}
          {previewState.status === "error" && (
            <div className="flex items-start gap-2 p-4 text-[12px] text-error min-w-0">
              <AlertTriangle size={13} className="shrink-0 mt-0.5" />
              <span className="break-words [overflow-wrap:anywhere] whitespace-pre-wrap min-w-0 flex-1">{previewState.message}</span>
            </div>
          )}
          {previewState.status === "binary" && (
            <div className="text-[12px] text-text-muted p-4 text-center">
              二进制文件（{formatBytes(previewState.size)}），不支持文本预览
            </div>
          )}
          {previewState.status === "empty" && (
            <div className="code-empty">空文件</div>
          )}
          {previewState.status === "text" && codeLang && (
            <CodeView
              source={textContent}
              language={codeLang}
              activeLine={activeLine + 1}
              onLineClick={(line) => {
                setActiveLine(line - 1);
                setCursor({ line, col: 1 });
              }}
            />
          )}
          {previewState.status === "text" && !codeLang && (
            <pre className="font-sans text-[13px] leading-relaxed text-text-primary whitespace-pre-wrap break-words p-4" style={{ margin: 0 }}>
              {previewState.content}
            </pre>
          )}
          {previewState.status === "office" && (
            <div className="w-full h-full" style={{ background: "#fff" }}>
              <iframe
                srcDoc={previewState.htmlContent}
                className="w-full h-full border-0 block"
                title={previewState.fileName}
                sandbox="allow-same-origin"
              />
            </div>
          )}
          {previewState.status === "spreadsheet" && (
            <SpreadsheetView
              model={previewState.model}
              fileName={previewState.fileName}
            />
          )}
          {previewState.status === "image" && (
            <div className="preview-image-stage">
              <img
                src={previewState.dataUrl}
                alt={artifact?.name ?? "image"}
                className="preview-image-img"
                draggable={false}
              />
            </div>
          )}
          {previewState.status === "pdf" && (
            <embed
              src={previewState.blobUrl}
              type="application/pdf"
              className="w-full h-full border-0 block"
              title={previewState.fileName}
            />
          )}
          {previewState.status === "markdown" && (
            <div className="preview-md-host">
              <article className="preview-md-body">
                <ReactMarkdown
                  remarkPlugins={[remarkGfm]}
                  components={{
                    // External links should open in the OS browser
                    // via Tauri, not as a file:// in the webview.
                    a: ({ href, children, ...rest }) => (
                      <a
                        href={href}
                        onClick={(e) => {
                          if (href && /^https?:\/\//i.test(href)) {
                            e.preventDefault();
                            openExternalUrl(href);
                          }
                        }}
                        target="_blank"
                        rel="noopener noreferrer"
                        {...rest}
                      >
                        {children}
                      </a>
                    ),
                  }}
                >
                  {previewState.content}
                </ReactMarkdown>
              </article>
            </div>
          )}
        </div>

        {/* Status bar — language · encoding · cursor */}
        {previewState.status === "text" && (
          <div className="code-status">
            <span className="cs-item cs-lang">{langLabel}</span>
            <span className="cs-item">UTF-8</span>
            <span className="cs-spacer" />
            <span className="cs-item">Ln {cursor.line}, Col {cursor.col}</span>
          </div>
        )}
        {previewState.status === "spreadsheet" && (
          <div className="code-status">
            <span className="cs-item cs-lang">{modelLabel(previewState.model)}</span>
            <span className="cs-item">UTF-8</span>
            <span className="cs-spacer" />
            <span className="cs-item">{totalRows(previewState.model)} 行</span>
          </div>
        )}
        {previewState.status === "image" && (
          <div className="code-status">
            <span className="cs-item cs-lang">{previewState.mime.replace("image/", "").toUpperCase()}</span>
            <span className="cs-item">UTF-8</span>
            <span className="cs-spacer" />
            <span className="cs-item">{formatBytes(previewState.size)}</span>
          </div>
        )}
        {previewState.status === "markdown" && (
          <div className="code-status">
            <span className="cs-item cs-lang">MARKDOWN</span>
            <span className="cs-item">UTF-8</span>
            <span className="cs-spacer" />
            <span className="cs-item">{previewState.content.length} 字符</span>
          </div>
        )}
      </div>

      {/* Fullscreen overlay via Portal — v4: simplified shell, no
            nested preview-pane wrappers (they were forcing flex-column
            padding:0 on the header inside the portal and breaking layout). */}
      {fullscreen && artifact && createPortal(
        <div
          className="preview-fullscreen"
          role="dialog"
          aria-modal="true"
          aria-label={`全屏预览：${artifact.name}`}
        >
          {renderHeader(true)}
          <div className="preview-fullscreen-body">
            <div className="preview-fullscreen-scroll">
              {previewState.status === "text" && codeLang && (
                <CodeView
                  source={textContent}
                  language={codeLang}
                  activeLine={activeLine + 1}
                  onLineClick={(line) => {
                    setActiveLine(line - 1);
                    setCursor({ line, col: 1 });
                  }}
                />
              )}
              {previewState.status === "text" && !codeLang && (
                <pre className="font-sans text-[13px] leading-relaxed text-text-primary whitespace-pre-wrap break-words p-6" style={{ margin: 0 }}>
                  {previewState.content}
                </pre>
              )}
              {previewState.status === "office" && (
                <div className="w-full h-full" style={{ background: "#fff" }}>
                  <iframe
                    srcDoc={previewState.htmlContent}
                    className="w-full h-full border-0 block"
                    title={previewState.fileName}
                    sandbox="allow-same-origin"
                  />
                </div>
              )}
              {previewState.status === "spreadsheet" && (
                <SpreadsheetView
                  model={previewState.model}
                  fileName={previewState.fileName}
                />
              )}
              {previewState.status === "error" && (
                <div className="flex items-start gap-2 p-6 text-[12px] text-error min-w-0">
                  <AlertTriangle size={13} className="shrink-0 mt-0.5" />
                  <span className="break-words [overflow-wrap:anywhere] whitespace-pre-wrap min-w-0 flex-1">{previewState.message}</span>
                </div>
              )}
              {previewState.status === "binary" && (
                <div className="text-[12px] text-text-muted p-6 text-center">二进制文件，不支持文本预览</div>
              )}
              {previewState.status === "empty" && (
                <div className="code-empty">空文件</div>
              )}
              {previewState.status === "loading" && (
                <div className="flex items-center justify-center h-full gap-2 text-[12px] text-text-muted">
                  <Loader2 size={13} className="animate-spin" />
                  正在读取文件…
                </div>
              )}
            </div>
            {previewState.status === "text" && (
              <div className="code-status">
                <span className="cs-item cs-lang">{langLabel}</span>
                <span className="cs-item">UTF-8</span>
                <span className="cs-spacer" />
                <span className="cs-item">Ln {cursor.line}, Col {cursor.col}</span>
              </div>
            )}
            {previewState.status === "spreadsheet" && (
              <div className="code-status">
                <span className="cs-item cs-lang">{modelLabel(previewState.model)}</span>
                <span className="cs-item">UTF-8</span>
                <span className="cs-spacer" />
                <span className="cs-item">{totalRows(previewState.model)} 行</span>
              </div>
            )}
          </div>
        </div>,
        document.body,
      )}
    </div>
  );
}

export function RightSidebar({ open, onClose, artifacts, selectedId, onSelectArtifact, workspaceRoot, rawCount, onAddFileToChat }: RightSidebarProps) {
  const [activeTab, setActiveTab] = useState<RightTab>("products");
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

  // Deselect when filtered selection disappears.
  //
  // Was previously done inline in the render body which triggered
  //   "Cannot update a component (`App`) while rendering a
  //    different component (`RightSidebar`)"
  // because onSelectArtifact ultimately calls setState on App. The
  // setState-during-render phase must run as an effect so it fires
  // after the current render commits.
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
        >
          <X size={13} />
        </button>
      </div>

      {/* Tab content */}
      {activeTab === "computer" && (
        <ComputerTab workspaceRoot={workspaceRoot} onAddToChat={onAddFileToChat} />
      )}

      {activeTab === "products" && (
        <div className="flex-1 flex flex-col overflow-hidden">
          {/* Search + filter */}
          <div className="px-3 pb-3 space-y-2">
            <div className="flex items-center gap-2 px-2.5 py-1.5 rounded-lg bg-bg-tertiary focus-within:ring-1 focus-within:ring-accent/30 transition">
              <Search size={12} className="text-text-muted shrink-0" />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
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
                    onClick={() => setFilter(f.key)}
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
          <div className="flex-1 overflow-y-auto border-t border-border-subtle px-3 py-2">
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

// ── Computer Tab (File Explorer) ──────────────────────────

interface FileNode {
  name: string;
  path: string;
  isDir: boolean;
  children?: FileNode[];
}

function ComputerTab({ workspaceRoot, onAddToChat }: { workspaceRoot?: string | null; onAddToChat?: (path: string) => void }) {
  const [expandedDirs, setExpandedDirs] = useState<Set<string>>(new Set());
  const [files, setFiles] = useState<FileNode[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const rootPath = workspaceRoot || "~";

  // Load directory contents using Tauri FS API
  const loadDirectory = useCallback(async (path: string) => {
    setLoading(true);
    setError(null);
    try {
      const { readDir } = await import("@tauri-apps/plugin-fs");
      const entries = await readDir(path);
      const fileNodes: FileNode[] = entries
        .filter((e) => !e.name.startsWith(".")) // Skip hidden files
        .map((e) => ({
          name: e.name,
          path: `${path}/${e.name}`,
          isDir: e.isDirectory,
        }))
        .sort((a, b) => {
          // Directories first, then files
          if (a.isDir && !b.isDir) return -1;
          if (!a.isDir && b.isDir) return 1;
          return a.name.localeCompare(b.name);
        });
      setFiles(fileNodes);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  // Toggle directory expansion
  const toggleDir = useCallback((path: string) => {
    setExpandedDirs((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
        loadDirectory(path);
      }
      return next;
    });
  }, [loadDirectory]);

  // Load root directory on mount
  useEffect(() => {
    loadDirectory(rootPath);
  }, [rootPath, loadDirectory]);

  return (
    <div className="flex-1 overflow-y-auto">
      {/* Workspace root indicator */}
      <div className="px-3 py-2 border-b border-border-primary">
        <div className="flex items-center gap-1.5 text-xs text-text-muted">
          <FolderOpen size={12} />
          <span className="truncate">{rootPath}</span>
        </div>
      </div>

      {/* File tree */}
      <div className="px-2 py-2">
        {loading && (
          <div className="flex items-center gap-2 px-2 py-4 text-xs text-text-muted">
            <Loader2 size={12} className="animate-spin" />
            加载中...
          </div>
        )}
        {error && (
          <div className="flex items-center gap-2 px-2 py-4 text-xs text-red-400">
            <AlertTriangle size={12} />
            {error}
          </div>
        )}
        {!loading && !error && files.length === 0 && (
          <div className="px-2 py-4 text-xs text-text-muted text-center">
            空目录
          </div>
        )}
        {files.map((file) => (
          <FileTreeItem
            key={file.path}
            file={file}
            expanded={expandedDirs.has(file.path)}
            onToggle={toggleDir}
            onAddToChat={onAddToChat}
          />
        ))}
      </div>
    </div>
  );
}

// ── File Tree Item Component ──────────────────────────────

function FileTreeItem({
  file,
  expanded,
  onToggle,
  onAddToChat,
}: {
  file: FileNode;
  expanded: boolean;
  onToggle: (path: string) => void;
  onAddToChat?: (path: string) => void;
}) {
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);

  const handleClick = () => {
    if (file.isDir) {
      onToggle(file.path);
    }
  };

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    setContextMenu({ x: e.clientX, y: e.clientY });
  };

  const handleAction = (action: string) => {
    setContextMenu(null);
    switch (action) {
      case "add_to_chat":
        onAddToChat?.(file.path);
        break;
      case "copy_path":
        navigator.clipboard.writeText(file.path);
        break;
      case "open_in_terminal":
        // TODO: Open terminal at file location
        console.log("Open in terminal:", file.path);
        break;
      case "preview":
        // TODO: Open file preview
        console.log("Preview:", file.path);
        break;
    }
  };

  return (
    <>
      <div
        onClick={handleClick}
        onContextMenu={handleContextMenu}
        className={clsx(
          "flex items-center gap-1.5 px-2 py-1 rounded-md text-xs cursor-pointer transition-colors",
          "hover:bg-bg-hover text-text-primary",
          file.isDir && "font-medium"
        )}
      >
        {file.isDir ? (
          expanded ? (
            <ChevronDown size={12} className="text-text-muted shrink-0" />
          ) : (
            <ChevronRight size={12} className="text-text-muted shrink-0" />
          )
        ) : (
          <File size={12} className="text-text-muted shrink-0" />
        )}
        <span className="truncate">{file.name}</span>
      </div>

      {/* Context Menu */}
      {contextMenu && (
        <ContextMenu
          x={contextMenu.x}
          y={contextMenu.y}
          onClose={() => setContextMenu(null)}
          onAction={handleAction}
          isDir={file.isDir}
        />
      )}
    </>
  );
}

// ── Context Menu Component ──────────────────────────────

function ContextMenu({
  x,
  y,
  onClose,
  onAction,
  isDir,
}: {
  x: number;
  y: number;
  onClose: () => void;
  onAction: (action: string) => void;
  isDir: boolean;
}) {
  useEffect(() => {
    const handleClick = () => onClose();
    document.addEventListener("click", handleClick);
    return () => document.removeEventListener("click", handleClick);
  }, [onClose]);

  const items = isDir
    ? [
        { action: "open_in_terminal", label: "在终端中打开", icon: Terminal },
        { action: "copy_path", label: "复制路径", icon: Copy },
      ]
    : [
        { action: "add_to_chat", label: "加入对话", icon: FileText },
        { action: "preview", label: "预览", icon: Eye },
        { action: "copy_path", label: "复制路径", icon: Copy },
      ];

  return (
    <div
      className="fixed z-50 bg-bg-card border border-border-primary rounded-lg shadow-lg py-1 min-w-[160px]"
      style={{ left: x, top: y }}
    >
      {items.map((item) => (
        <button
          key={item.action}
          onClick={() => onAction(item.action)}
          className="flex items-center gap-2 w-full px-3 py-1.5 text-xs text-text-primary hover:bg-bg-hover transition-colors"
        >
          <item.icon size={12} />
          {item.label}
        </button>
      ))}
    </div>
  );
}

// ── Browser Tab (Placeholder) ──────────────────────────────

function BrowserTab() {
  const [url, setUrl] = useState("");
  const [status, setStatus] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const [pageTitle, setPageTitle] = useState<string | null>(null);
  const [history, setHistory] = useState<string[]>([]);

  const handleNavigate = async () => {
    if (!url.trim()) return;
    setStatus("loading");
    try {
      // TODO: Call browser_navigate tool via Tauri
      setPageTitle(`页面: ${url}`);
      setHistory((prev) => [url, ...prev].slice(0, 20));
      setStatus("ready");
    } catch (e) {
      setStatus("error");
    }
  };

  const handleScreenshot = async () => {
    // TODO: Call browser_snapshot tool
    console.log("Take screenshot");
  };

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      {/* URL Input */}
      <div className="px-3 py-3 border-b border-border-primary">
        <div className="flex items-center gap-2">
          <input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleNavigate()}
            placeholder="输入网址..."
            className="flex-1 px-3 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary placeholder:text-text-faint outline-none focus:ring-1 focus:ring-accent/30"
          />
          <button
            onClick={handleNavigate}
            className="px-3 py-1.5 rounded-lg bg-accent text-white text-xs font-medium hover:opacity-90 transition-opacity"
          >
            前往
          </button>
        </div>
      </div>

      {/* Status Bar */}
      <div className="px-3 py-2 border-b border-border-primary flex items-center gap-2">
        <div className={`w-2 h-2 rounded-full ${
          status === "ready" ? "bg-green-400" :
          status === "loading" ? "bg-yellow-400 animate-pulse" :
          status === "error" ? "bg-red-400" : "bg-gray-400"
        }`} />
        <span className="text-[11px] text-text-muted">
          {status === "ready" ? pageTitle || "就绪" :
           status === "loading" ? "加载中..." :
           status === "error" ? "错误" : "未连接"}
        </span>
        {status === "ready" && (
          <button
            onClick={handleScreenshot}
            className="ml-auto px-2 py-0.5 rounded text-[10px] text-text-muted hover:bg-bg-hover transition-colors"
          >
            📸 截图
          </button>
        )}
      </div>

      {/* Quick Actions */}
      <div className="px-3 py-3 space-y-3">
        <div className="text-xs font-medium text-text-primary">快捷操作</div>
        <div className="grid grid-cols-2 gap-2">
          <QuickAction
            icon={<Search size={14} />}
            label="搜索页面"
            onClick={() => console.log("Search")}
          />
          <QuickAction
            icon={<Eye size={14} />}
            label="元素检查"
            onClick={() => console.log("Inspect")}
          />
          <QuickAction
            icon={<Copy size={14} />}
            label="复制内容"
            onClick={() => console.log("Copy")}
          />
          <QuickAction
            icon={<FileText size={14} />}
            label="提取文本"
            onClick={() => console.log("Extract")}
          />
        </div>
      </div>

      {/* History */}
      {history.length > 0 && (
        <div className="px-3 py-3 border-t border-border-primary">
          <div className="text-xs font-medium text-text-primary mb-2">访问历史</div>
          <div className="space-y-1">
            {history.map((h, i) => (
              <button
                key={i}
                onClick={() => setUrl(h)}
                className="block w-full text-left px-2 py-1 rounded text-[11px] text-text-muted hover:bg-bg-hover truncate transition-colors"
              >
                {h}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ── Quick Action Component ──────────────────────────────

function QuickAction({
  icon,
  label,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="flex items-center gap-2 px-3 py-2 rounded-lg bg-bg-tertiary hover:bg-bg-hover text-xs text-text-primary transition-colors"
    >
      {icon}
      {label}
    </button>
  );
}
