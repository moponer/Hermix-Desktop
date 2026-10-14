// 预览面板的壳：判定 → 加载 → 把结果交给 PreviewSurface 渲染。
//
// 这个组件只负责三件事：头部工具栏、加载状态机、全屏容器。
// 所有"某种文件长什么样"的知识都在 PreviewSurface / viewers 里，
// 且面板与全屏共用同一份渲染，不存在第二处 switch。

import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import {
  X, Copy, ExternalLink, FolderOpen,
  Loader2, AlertTriangle, WrapText, FileText, Code2, Maximize2, Minimize2,
} from "lucide-react";
import { clsx } from "clsx";
import {
  loadPreview,
  PreviewSurface,
  applyViewMode,
  resolveKind,
  supportsTextView,
  supportsZoom,
  type ArtifactKindHint,
  type PreviewPayload,
  type PreviewState,
  type ViewMode,
} from "@hermix/preview";
import type { Artifact } from "../utils/artifacts";
import { openExternalUrl } from "../desktopBridge";
import { desktopFileSource } from "../lib/fileSourceAdapter";
import { CodeView } from "./CodeView";
import { openImageViewer } from "./imageViewer";
import { FileTypeIcon } from "./FileTypeIcon";
import { formatBytes, fileTypeLabel, totalRows, modelLabel } from "./rightSidebarUtils";

const ZOOM_MIN = 50;
const ZOOM_MAX = 200;

/** ArtifactKind → 包侧 resolveKind 的 hint：后端给老二进制 Office 打的
 *  "office" 标签在包里叫 legacy（走 markitdown 文本路径）。 */
function kindHint(artifact: Artifact | null): ArtifactKindHint | undefined {
  return artifact?.kind === "office" ? { legacy: true } : undefined;
}

/** 代码视图的行号由面板持有（状态栏要显示 Ln/Col），这里只做受控渲染。
 *  经 PreviewSurface 的 renderCode 注入口喂给共享渲染层。 */
function CodeSurface({
  content,
  language,
  wrap,
  isDark,
  cursor,
  onCursorChange,
}: {
  content: string;
  language: string | null;
  wrap: boolean;
  isDark: boolean;
  cursor: { line: number; col: number };
  onCursorChange: (c: { line: number; col: number }) => void;
}) {
  if (!language) {
    return <pre className="preview-plain-text">{content}</pre>;
  }
  return (
    <CodeView
      source={content}
      language={language}
      wrap={wrap}
      isDark={isDark}
      activeLine={cursor.line}
      onLineClick={(line) => onCursorChange({ line, col: 1 })}
    />
  );
}

/** 状态栏右段的补充信息，按载荷种类给出。 */
function statusExtra(payload: PreviewPayload): string | null {
  switch (payload.kind) {
    case "image":
      return formatBytes(payload.size);
    case "pdf":
    case "docx":
    case "pptx":
      return formatBytes(payload.bytes.byteLength);
    case "markdown":
    case "html":
      return `${payload.content.length} 字符`;
    case "spreadsheet":
      return `${totalRows(payload.model)} 行`;
    default:
      return null;
  }
}

/** 可复制到剪贴板的纯文本，没有则返回 null（工具栏不显示复制按钮）。 */
function copyableText(payload: PreviewPayload): string | null {
  switch (payload.kind) {
    case "code":
    case "text":
    case "markdown":
    // 网页本身也是一份可直接复制的源码，排版视图下也该给。
    case "html":
      return payload.content;
    default:
      return null;
  }
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
  const [state, setState] = useState<PreviewState>({ status: "idle" });
  const [resolvedPath, setResolvedPath] = useState<string | null>(null);
  const [fullscreen, setFullscreen] = useState(false);
  const [wrap, setWrap] = useState(true);
  const [viewMode, setViewMode] = useState<ViewMode>("layout");
  const [zoom, setZoom] = useState(100);
  const [cursor, setCursor] = useState({ line: 1, col: 1 });

  // FileSource 绑定 workspaceRoot（包加载器只收 path，root 在这里闭包进
  // artifactReader 的祖先回溯链）；root 变了才重建，避免每渲染一份新 source。
  const source = useMemo(() => desktopFileSource(workspaceRoot), [workspaceRoot]);

  // 换文件回到排版视图并复位光标，避免上一个文件的选择泄漏过来。
  useEffect(() => {
    setViewMode("layout");
    setCursor({ line: 1, col: 1 });
    setZoom(100);
  }, [artifact?.path]);

  useEffect(() => {
    if (!artifact?.path) {
      setState({ status: "idle" });
      setResolvedPath(null);
      return;
    }
    const kind = applyViewMode(resolveKind(artifact.name, kindHint(artifact)), viewMode);
    let cancelled = false;
    setState({ status: "loading", kind });

    loadPreview(
      {
        name: artifact.name,
        path: artifact.path,
        workspaceRoot,
        viewMode,
        kind,
      },
      source,
    )
      .then((out) => {
        if (cancelled) return;
        if (!out.ok) {
          setState({ status: "error", message: out.message });
          return;
        }
        setResolvedPath(out.resolvedPath ?? null);
        setState({
          status: "ready",
          kind: out.payload.kind,
          payload: out.payload,
          resolvedPath: out.resolvedPath ?? null,
        });
      })
      .catch((e: unknown) => {
        if (!cancelled) setState({ status: "error", message: String(e) });
      });

    return () => {
      cancelled = true;
    };
  }, [artifact?.path, artifact?.name, artifact?.kind, workspaceRoot, viewMode, source]);

  useEffect(() => {
    if (!fullscreen) return;
    const onKey = (e: KeyboardEvent) => {
      // 全屏优先消费 ESC：否则外层的产物预览模态框（监听 window）
      // 会同时收到它，一次按键既退全屏又关掉整个弹窗。
      if (e.key === "Escape") {
        e.stopPropagation();
        setFullscreen(false);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [fullscreen]);

  // 深色判定走 colorScheme（useTheme 设置）；.dark 类已废除，contains 检测永远 false
  const isDark =
    typeof document !== "undefined" &&
    document.documentElement.style.colorScheme === "dark";

  if (!artifact) return null;

  const baseKind = resolveKind(artifact.name, kindHint(artifact));
  const cleanPath = (resolvedPath ?? artifact.path).replace(/^\\\\\?\\/, "");
  const openContainingDir = () => {
    const dir = cleanPath.replace(/[/\\][^/\\]+$/, "");
    openExternalUrl(dir);
  };

  const payload = state.status === "ready" ? state.payload : null;
  const copyText = payload ? copyableText(payload) : null;

  const renderHeader = (isFs: boolean) => (
    <div className={clsx("preview-head", isFs && "is-fullscreen")}>
      <div className="preview-head-row1">
        <div
          className="file-icon-tile file-icon-tile--branded"
          style={{ width: isFs ? 44 : 40, height: isFs ? 44 : 40, background: "transparent", boxShadow: "none" }}
          title={artifact.name}
        >
          <FileTypeIcon name={artifact.name} size={isFs ? 44 : 40} style={{ display: "block" }} />
        </div>

        <span className="preview-filename">{artifact.name}</span>
        <span className="preview-lang">
          <span className="preview-lang-dot" />
          {fileTypeLabel(artifact.name)}
        </span>

        <div className="preview-head-spacer" />

        <button
          type="button"
          onClick={() => setFullscreen((f) => !f)}
          title={fullscreen ? "退出全屏" : "全屏预览"}
          className="head-action"
          aria-label={fullscreen ? "退出全屏" : "全屏预览"}
        >
          {fullscreen ? <Minimize2 className="icon-md" /> : <Maximize2 className="icon-md" />}
        </button>
        <button
          type="button"
          onClick={onBack}
          title="关闭预览"
          className="head-action is-danger"
          aria-label="关闭预览"
        >
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
          {supportsTextView(baseKind) && (
            <button
              type="button"
              onClick={() => setViewMode((m) => (m === "text" ? "layout" : "text"))}
              title={
                baseKind === "html"
                  ? viewMode === "text" ? "回到页面视图" : "查看源码"
                  : viewMode === "text" ? "回到排版视图" : "文本视图（可搜索 / 可复制）"
              }
              className={clsx("head-action", viewMode === "text" && "is-active")}
              aria-label={baseKind === "html" ? "切换源码视图" : "切换文本视图"}
            >
              {baseKind === "html" ? <Code2 className="icon-md" /> : <FileText className="icon-md" />}
            </button>
          )}
          {supportsZoom(payload?.kind ?? baseKind) && viewMode === "layout" && (
            <div className="preview-zoom" role="group" aria-label="预览缩放">
              <button
                type="button"
                className="head-action"
                onClick={() => setZoom((z) => Math.max(ZOOM_MIN, z - 10))}
                disabled={zoom <= ZOOM_MIN}
                title="缩小"
                aria-label="缩小预览"
              >
                −
              </button>
              <span className="preview-zoom-val">{zoom}%</span>
              <button
                type="button"
                className="head-action"
                onClick={() => setZoom((z) => Math.min(ZOOM_MAX, z + 10))}
                disabled={zoom >= ZOOM_MAX}
                title="放大"
                aria-label="放大预览"
              >
                ＋
              </button>
            </div>
          )}
          {payload?.kind === "code" && payload.language && (
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
          {copyText !== null && (
            <button
              type="button"
              onClick={() => void navigator.clipboard.writeText(copyText)}
              title="复制内容"
              className="head-action"
              aria-label="复制内容"
            >
              <Copy className="icon-md" />
            </button>
          )}
          <button
            type="button"
            onClick={() => openExternalUrl(resolvedPath ?? artifact.path)}
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

  const renderBody = () => {
    if (state.status === "loading" || state.status === "idle") {
      return (
        <div className="preview-state-center">
          <Loader2 className="icon-md animate-spin" />
          正在读取文件…
        </div>
      );
    }
    if (state.status === "error") {
      return (
        <div className="preview-state-error-row">
          <AlertTriangle className="icon-md shrink-0 mt-0.5" />
          <span>{state.message}</span>
        </div>
      );
    }
    return (
      <PreviewSurface
        payload={state.payload}
        source={source}
        isDark={isDark}
        fileName={artifact.name}
        resolvedPath={resolvedPath}
        zoom={zoom}
        onOpenExternalUrl={openExternalUrl}
        renderCode={(content, language) => (
          <CodeSurface
            content={content}
            language={language}
            wrap={wrap}
            isDark={isDark}
            cursor={cursor}
            onCursorChange={setCursor}
          />
        )}
        onOpenImage={(bytes, mime, name) =>
          openImageViewer({
            // TS 5.7+ 的 Uint8Array 带泛型：ArrayBufferLike ≠ BlobPart 要求的
            // ArrayBuffer。bytes 来自自家 artifact reader（全新分配、offset 0），
            // cast 安全。
            src: URL.createObjectURL(new Blob([bytes as unknown as BlobPart], { type: mime })),
            name,
          })
        }
      />
    );
  };

  const renderStatus = () => {
    if (state.status !== "ready") return null;
    const extra = statusExtra(state.payload);
    return (
      <div className="code-status">
        <span className="cs-item cs-lang">
          {state.payload.kind === "spreadsheet"
            ? modelLabel(state.payload.model)
            : fileTypeLabel(artifact.name)}
        </span>
        {state.payload.kind === "markdown" && state.payload.truncated && (
          <span className="cs-item">已截断至 200K 字符</span>
        )}
        <span className="cs-spacer" />
        {state.payload.kind === "code" ? (
          <span className="cs-item">
            Ln {cursor.line}, Col {cursor.col}
          </span>
        ) : (
          extra && <span className="cs-item">{extra}</span>
        )}
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
          {/* 全屏时面板这一份不参与渲染：同一载荷不画两遍（PDF / PPTX 都是 canvas） */}
          <div className="preview-pane-inner flex-1 overflow-auto min-h-0">
            {!fullscreen && renderBody()}
          </div>
        {renderStatus()}
      </div>

      {fullscreen &&
        createPortal(
          <div
            className="preview-fullscreen preview-pane"
            role="dialog"
            aria-modal="true"
            aria-label={`全屏预览：${artifact.name}`}
          >
            {renderHeader(true)}
            <div className="preview-fullscreen-body">
              <div className="preview-fullscreen-scroll">{renderBody()}</div>
              {renderStatus()}
            </div>
          </div>,
          document.body,
        )}
    </div>
  );
}
