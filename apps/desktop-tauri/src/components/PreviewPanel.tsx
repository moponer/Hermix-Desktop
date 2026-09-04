import { useEffect, useMemo, useRef, useState } from "react";
import { openImageViewer } from "./imageViewer";
import { createPortal } from "react-dom";
import {
  X, Copy, ExternalLink, FolderOpen,
  Loader2, AlertTriangle, WrapText,
} from "lucide-react";
import { markdownToHast } from "satteri";
import { toJsxRuntime } from "hast-util-to-jsx-runtime";
import { Fragment, jsx, jsxs } from "react/jsx-runtime";
import type { ReactNode } from "react";
import { hardenSoftLineBreaks } from "../lib/softBreaks";
import { CodeView } from "./CodeView";
import { CodeBlock } from "./MarkdownBody";
import { SpreadsheetView } from "./SpreadsheetView";
import { FileTypeIcon } from "./FileTypeIcon";
import { clsx } from "clsx";
import type { Artifact } from "../utils/artifacts";
import { openExternalUrl } from "../desktopBridge";
import {
  tryReadArtifact,
  tryReadArtifactBytes,
  tryRenderOfficePreview,
} from "../utils/artifactReader";
import { parseByExtension, type PreviewModel } from "../lib/fileParser";
import {
  formatBytes, IMAGE_EXT_MIME, IMAGE_BYTE_LIMIT,
  isImageExt, isPdfExt, isMarkdownExt, needsFrontendParse,
  detectLanguage, LANG_LABELS, totalRows, modelLabel,
} from "./rightSidebarUtils";

type PreviewState =
  | { status: "idle" }
  | { status: "loading" }
  | { status: "text"; content: string }
  | { status: "binary"; size: number }
  | { status: "empty" }
  | { status: "error"; message: string }
  | { status: "office"; htmlContent: string; fileName: string }
  | { status: "spreadsheet"; model: PreviewModel; fileName: string }
  | { status: "image"; dataUrl: string; mime: string; size: number; width?: number; height?: number }
  | { status: "markdown"; content: string; fileName: string }
  | { status: "pdf"; blobUrl: string; fileName: string };

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
  const [wrap, setWrap] = useState(true);
  const [activeLine, setActiveLine] = useState(0);
  const [cursor, setCursor] = useState<{ line: number; col: number }>({ line: 1, col: 1 });

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
      if (prevBlobRef.current) {
        URL.revokeObjectURL(prevBlobRef.current);
        prevBlobRef.current = null;
      }
    };
  }, [previewState]);

  useEffect(() => {
    if (!artifact?.path) {
      setPreviewState({ status: "idle" });
      setResolvedPath(null);
      return;
    }

    let cancelled = false;
    setPreviewState({ status: "loading" });

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

  // 深色判定走 colorScheme（useTheme 设置）；.dark 类已废除，contains 检测永远 false
  const isDark =
    typeof document !== "undefined" &&
    document.documentElement.style.colorScheme === "dark";

  const openContainingDir = () => {
    if (!artifact) return;
    const target = (resolvedPath ?? artifact.path).replace(/^\\\\\?\\/, "");
    const dir = target.replace(/[/\\][^/\\]+$/, "");
    openExternalUrl(dir);
  };

  useEffect(() => {
    if (!fullscreen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setFullscreen(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [fullscreen]);

  const lang = artifact ? detectLanguage(artifact.name) : null;
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

  useEffect(() => {
    setActiveLine(0);
    setCursor({ line: 1, col: 1 });
  }, [artifact?.path]);

  const renderHeader = (isFs: boolean) => {
    if (!artifact) return null;
    return (
      <div className={clsx("preview-head", isFs && "is-fullscreen")}>
        <div className="preview-head-row1">
          {(() => {
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
            aria-label="关闭预览">
            <X className="icon-md" />
          </button>
        </div>

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
            {(previewState.status === "text") && (
              <button
                type="button"
                onClick={() => setWrap((w) => !w)}
                title={wrap ? "取消自动换行" : "开启自动换行"}
                className={clsx("head-action", wrap && "is-active")}
                aria-label={wrap ? "取消自动换行" : "开启自动换行"}
              >
                <WrapText className="icon-md" />
              </button>
            )}
            {(previewState.status === "text" || previewState.status === "markdown") && (
              <button
                type="button"
                onClick={() => {
                  const content = previewState.status === "text"
                    ? previewState.content
                    : previewState.status === "markdown"
                      ? previewState.content
                      : "";
                  if (content) {
                    navigator.clipboard.writeText(content);
                  }
                }}
                title="复制内容"
                className="head-action"
                aria-label="复制内容"
              >
                <Copy className="icon-md" />
              </button>
            )}
            <button
              type="button"
              onClick={() => {
                if (artifact) openExternalUrl(resolvedPath ?? artifact.path);
              }}
              title="用默认程序打开"
              className="head-action"
              aria-label="用默认程序打开"
            >
              <ExternalLink className="icon-md" />
            </button>
            <button
              type="button"
              onClick={openContainingDir}
              title="在资源管理器中打开"
              className="head-action"
              aria-label="在资源管理器中打开"
            >
              <FolderOpen className="icon-md" />
            </button>
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className="flex flex-col h-full min-h-0 overflow-hidden preview-pane">
      {renderHeader(false)}

      <div
        className="flex-1 flex flex-col min-h-0 preview-body"
        style={{ background: "var(--color-bg-code)" }}
      >
        <div className="preview-pane-inner flex-1 overflow-auto min-h-0">
          {previewState.status === "loading" && (
            <div className="flex items-center justify-center h-full gap-2 text-[12px] text-text-muted preview-loading">
              <Loader2 className="icon-md animate-spin" />
              正在读取文件…
            </div>
          )}
          {previewState.status === "error" && (
            <div className="flex items-start gap-2 p-4 text-[12px] text-error min-w-0">
              <AlertTriangle className="icon-md shrink-0 mt-0.5" />
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
              wrap={wrap}
              isDark={isDark}
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
            <div className="w-full h-full" style={{ background: "var(--color-bg-primary)" }}>
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
                onClick={() => openImageViewer({ src: previewState.dataUrl, name: artifact?.name })}
                style={{ cursor: "pointer" }}
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
                <MarkdownPreview
                  content={previewState.content}
                  isDark={isDark}
                />
              </article>
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
                  wrap={wrap}
                  isDark={isDark}
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
                <div className="w-full h-full" style={{ background: "var(--color-bg-primary)" }}>
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
                  <AlertTriangle className="icon-md shrink-0 mt-0.5" />
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
                  <Loader2 className="icon-md animate-spin" />
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

/* ── Markdown preview (satteri pipeline) ──────────────────────── */

// 内联轻量组件：satteri + hast-util-to-jsx-runtime 渲染 .md 文件预览。
// 与 MarkdownBody 的差别：
//   - 不接 PathLink（preview 本地文件，自有 onClick 路由）
//   - 链接拦截外部 https:// 让 openExternalUrl 走系统浏览器
//   - 不需要 streaming 节流（preview 一次性读全文）
function MarkdownPreview({ content, isDark }: { content: string; isDark: boolean }) {
  const rendered = useMemo<ReactNode>(() => {
    const hardened = hardenSoftLineBreaks(content);
    const hast = markdownToHast(hardened, { features: { gfm: true } });
    return toJsxRuntime(hast, {
      Fragment,
      jsx,
      jsxs,
      components: {
        a({ href, children, ...rest }: { href?: string; children?: ReactNode }) {
          return (
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
          );
        },
        code({ className, children, ...props }: { className?: string; children?: ReactNode }) {
          const match = /language-([\w-]+)/.exec(className || "");
          const codeString = String(children ?? "").replace(/\n$/, "");
          if (match) {
            return <CodeBlock language={match[1]!.toLowerCase()} code={codeString} isDark={isDark} />;
          }
          return <code {...props}>{children}</code>;
        },
      },
    });
  }, [content, isDark]);
  return <>{rendered}</>;
}
