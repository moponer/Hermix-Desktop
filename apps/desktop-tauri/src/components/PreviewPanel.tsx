// 预览面板的壳：判定 → 加载 → 把结果交给 PreviewSurface 渲染。
//
// 这个组件只负责三件事：头部工具栏、加载状态机、全屏容器。
// 所有"某种文件长什么样"的知识都在 PreviewSurface / viewers 里，
// 且面板与全屏共用同一份渲染，不存在第二处 switch。

import { useEffect, useState } from "react";
import { createPortal } from "react-dom";
import {
  X, Copy, ExternalLink, FolderOpen,
  Loader2, AlertTriangle, WrapText, FileText, Maximize2, Minimize2,
} from "lucide-react";
import { clsx } from "clsx";
import type { Artifact } from "../utils/artifacts";
import { openExternalUrl } from "../desktopBridge";
import { FileTypeIcon } from "./FileTypeIcon";
import { formatBytes, fileTypeLabel, totalRows, modelLabel } from "./rightSidebarUtils";
import { resolveKind, applyViewMode } from "./preview/resolveKind";
import { LOADERS } from "./preview/loaders";
import { PreviewSurface } from "./preview/PreviewSurface";
import { supportsTextView, type PreviewPayload, type PreviewState, type ViewMode } from "./preview/types";

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
  const [cursor, setCursor] = useState({ line: 1, col: 1 });

  // 换文件回到排版视图并复位光标，避免上一个文件的选择泄漏过来。
  useEffect(() => {
    setViewMode("layout");
    setCursor({ line: 1, col: 1 });
  }, [artifact?.path]);

  useEffect(() => {
    if (!artifact?.path) {
      setState({ status: "idle" });
      setResolvedPath(null);
      return;
    }
    const kind = applyViewMode(resolveKind(artifact.name, artifact.kind), viewMode);
    let cancelled = false;
    setState({ status: "loading", kind });

    LOADERS[kind]({
      name: artifact.name,
      path: artifact.path,
      workspaceRoot,
      viewMode,
    })
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
  }, [artifact?.path, artifact?.name, artifact?.kind, workspaceRoot, viewMode]);

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

  const baseKind = resolveKind(artifact.name, artifact.kind);
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
              title={viewMode === "text" ? "回到排版视图" : "文本视图（可搜索 / 可复制）"}
              className={clsx("head-action", viewMode === "text" && "is-active")}
              aria-label={viewMode === "text" ? "回到排版视图" : "文本视图"}
            >
              <FileText className="icon-md" />
            </button>
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
        wrap={wrap}
        isDark={isDark}
        fileName={artifact.name}
        cursor={cursor}
        onCursorChange={setCursor}
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
