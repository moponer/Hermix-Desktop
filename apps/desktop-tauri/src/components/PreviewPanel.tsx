// 预览面板的壳：判定 → 加载 → 把结果交给 PreviewSurface 渲染。
//
// 这个组件只负责三件事：头部工具栏、加载状态机、全屏容器。
// 所有"某种文件长什么样"的知识都在 PreviewSurface / viewers 里，
// 且面板与全屏共用同一份渲染，不存在第二处 switch。

import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { createPortal } from "react-dom";
import { useFocusTrap } from "../hooks/useFocusTrap";
import {
  X, Copy, ExternalLink, FolderOpen, MoreHorizontal,
  Loader2, AlertTriangle, WrapText, FileText, Code2, Maximize2, Minimize2,
} from "lucide-react";
import { clsx } from "clsx";
import { PopoverShell, PopoverBody } from "./PopoverShell";
import { usePopover } from "../hooks/usePopover";
import { toastSuccess, toastError } from "../hooks/useToast";
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
import { FileTypeIcon } from "@hermix/preview/FileTypeIcon";
import { formatBytes, fileTypeLabel, totalRows, modelLabel } from "./rightSidebarUtils";
import { COPIED, COPY_FAILED, LOADING, failure } from "./strings";

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

/** 预览头部的「更多」菜单：低频动作收进来，头部只留高频控件。
 *  每个头部实例各自持有状态与锚点（面板态 / 全屏态各一份），
 *  避免共用 ref 时把菜单锚到看不见的那一份上。 */
function PreviewMenu({
  copyText,
  showBrowser,
  onOpenBrowser,
  onOpenExternal,
  onOpenFolder,
}: {
  copyText: string | null;
  showBrowser: boolean;
  onOpenBrowser: () => void;
  onOpenExternal: () => void;
  onOpenFolder: () => void;
}) {
  const [open, , containerRef, triggerRef, toggle, close] = usePopover();
  const itemCls =
    "flex w-full items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[length:var(--text-ui)] text-text-secondary transition-colors hover:bg-bg-hover hover:text-text-primary";

  const copy = () => {
    if (copyText === null) return;
    void navigator.clipboard
      .writeText(copyText)
      .then(() => toastSuccess(COPIED))
      .catch(() => toastError(failure(COPY_FAILED, "请手动选中内容后复制。")));
  };

  return (
    <>
      <button
        ref={triggerRef as RefObject<HTMLButtonElement>}
        type="button"
        onClick={toggle}
        title="更多操作"
        aria-label="更多操作"
        aria-haspopup="menu"
        aria-expanded={open}
        className={clsx("head-action", open && "is-active")}
      >
        <MoreHorizontal className="icon-md" />
      </button>
      {open && (
        <PopoverShell
          containerRef={containerRef}
          triggerRef={triggerRef}
          align="right"
          side="bottom"
          width="sm"
        >
          <PopoverBody maxHeight="max-h-64" className="py-0.5">
            {copyText !== null && (
              <button type="button" className={itemCls} onClick={() => { copy(); close(); }}>
                <Copy className="icon-sm shrink-0 opacity-70" />
                复制内容
              </button>
            )}
            {showBrowser && (
              <button type="button" className={itemCls} onClick={() => { close(); onOpenBrowser(); }}>
                <ExternalLink className="icon-sm shrink-0 opacity-70" />
                用浏览器打开
              </button>
            )}
            <button type="button" className={itemCls} onClick={() => { close(); onOpenExternal(); }}>
              <ExternalLink className="icon-sm shrink-0 opacity-70" />
              用默认程序打开
            </button>
            <button type="button" className={itemCls} onClick={() => { close(); onOpenFolder(); }}>
              <FolderOpen className="icon-sm shrink-0 opacity-70" />
              在资源管理器中打开
            </button>
          </PopoverBody>
        </PopoverShell>
      )}
    </>
  );
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
  const fullscreenRef = useRef<HTMLDivElement>(null);
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

  // 全屏覆盖层声明了 aria-modal="true"，就必须有焦点陷阱 ——
  // 否则 Tab 一下焦点就跑到全屏层背后的内容上。与 ui/Modal 共用同一份实现。
  useFocusTrap(fullscreenRef, fullscreen);

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
      <div className="preview-head-main">
        <FileTypeIcon name={artifact.name} size={isFs ? 30 : 26} style={{ display: "block" }} />

        <div className="preview-head-titles">
          <div className="preview-head-title-row">
            <span className="preview-filename" title={artifact.name}>{artifact.name}</span>
            <span className="preview-lang">{fileTypeLabel(artifact.name)}</span>
          </div>
          <button
            type="button"
            className="preview-path"
            title={`打开所在目录：${cleanPath}`}
            onClick={openContainingDir}
          >
            {cleanPath}
          </button>
        </div>

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
          <button
            type="button"
            onClick={() => setFullscreen((f) => !f)}
            title={fullscreen ? "退出全屏" : "全屏预览"}
            className="head-action"
            aria-label={fullscreen ? "退出全屏" : "全屏预览"}
          >
            {fullscreen ? <Minimize2 className="icon-md" /> : <Maximize2 className="icon-md" />}
          </button>
          <PreviewMenu
            copyText={copyText}
            showBrowser={payload?.kind === "html"}
            onOpenBrowser={() => {
              if (resolvedPath) openExternalUrl(resolvedPath);
            }}
            onOpenExternal={() => openExternalUrl(resolvedPath ?? artifact.path)}
            onOpenFolder={openContainingDir}
          />
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
      </div>
    </div>
  );

  const renderBody = () => {
    if (state.status === "loading" || state.status === "idle") {
      return (
        <div className="preview-state-center" aria-busy="true">
          <Loader2 className="icon-md animate-spin" />
          {LOADING}
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
        onOpenImage={(bytes, mime, name) => {
          // 复制到 ArrayBuffer 支撑的 Uint8Array：TS 5.7+ 的 DOM 库把
          // BlobPart 收紧成 ArrayBufferView<ArrayBuffer>，而 FileSource 契约里
          // 的 bytes 是泛型默认的 Uint8Array<ArrayBufferLike>，直接传会被拒收。
          const copy = new Uint8Array(bytes.byteLength);
          copy.set(bytes);
          openImageViewer({
            src: URL.createObjectURL(new Blob([copy], { type: mime })),
            name,
          });
        }}
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
        className="flex-1 flex flex-col min-h-0 preview-body bg-bg-code"
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
            ref={fullscreenRef}
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
