// 预览宿主：在 viewer 中用共享预览管线（@hermix/preview）渲染文件。
// 一份 PreviewHost = 一个 tab 的正文。父组件负责调度 source 读、
// 失败重试、tab 关闭。
//
// 渲染走共享包，与桌面 app 完全一致：viewer / desktop 用同一份解析器，
// 共享一份样式 token。

import { useEffect, useMemo, useRef, useState } from "react";
import { lazy, Suspense } from "react";
import { Pencil, RotateCcw } from "lucide-react";
import {
  loadPreview,
  PreviewSurface,
  resolveKind,
  applyViewMode,
  type FileSource,
  type LoadOutcome,
  type PreviewState,
  supportsTextView,
  supportsZoom,
  type PreviewKind,
  type ViewMode,
} from "@hermix/preview";
import { isEditableImage } from "./image/imageOps";

// 图片编辑器懒加载：它是重量模式的重量功能，纯净模式与文本预览都不该
// 为它付首屏 bundle 的钱。
const ImageEditor = lazy(() =>
  import("./image/ImageEditor").then((m) => ({ default: m.ImageEditor })),
);

const ZOOM_MIN = 50;
const ZOOM_MAX = 200;

export function PreviewHost({
  path,
  name,
  source,
  isDark,
  canEdit = false,
}: {
  path: string;
  name: string;
  source: FileSource;
  isDark: boolean;
  /** 重量模式才给编辑能力（纯净模式是"双击就看"，不塞工具）。 */
  canEdit?: boolean;
}) {
  const [state, setState] = useState<PreviewState>({ status: "loading", kind: "text" });
  const [viewMode, setViewMode] = useState<ViewMode>("layout");
  const [zoom, setZoom] = useState(100);
  const [editing, setEditing] = useState(false);
  const loadToken = useRef(0);

  const kind: PreviewKind = useMemo(() => resolveKind(name), [name]);
  const effectiveKind = applyViewMode(kind, viewMode);

  // 加载阶段：每次 path / viewMode 变了重读
  useEffect(() => {
    const myToken = ++loadToken.current;
    let cancelled = false;
    setState({ status: "loading", kind: effectiveKind });
    const req = { name, path, workspaceRoot: null as string | null, viewMode };
    (async () => {
      const out: LoadOutcome = await loadPreview({ ...req, kind: effectiveKind }, source);
      if (cancelled || myToken !== loadToken.current) return;
      if (!out.ok) {
        setState({ status: "error", message: out.message });
        return;
      }
      setState({
        status: "ready",
        kind: out.payload.kind,
        payload: out.payload,
        resolvedPath: out.resolvedPath,
      });
    })();
    return () => {
      cancelled = true;
    };
  }, [name, path, viewMode, effectiveKind, source]);

  // 外壳（工具条 + 内容区）常驻：载入 / 错误态也走同一套壳，
  // 否则状态提示会掉到 .preview-body 左边（flex 行容器里宽度自适应），
  // 表现为"贴在左上角而不是整栏居中"。
  const readyState = state.status === "ready" ? state : null;
  const editable = canEdit && effectiveKind === "image" && isEditableImage(name);

  return (
    <div className="preview-pane">
      <div className="preview-toolbar">
        <span className="preview-toolbar-name" title={path}>{name}</span>
        <span className="preview-toolbar-spacer" />
        {editable && !editing && (
          <button
            type="button"
            className="tb-btn"
            onClick={() => setEditing(true)}
            title="编辑图片（裁剪 / 调整 / 转换格式）"
            aria-label="编辑图片"
          >
            <Pencil className="icon-sm" />
            编辑
          </button>
        )}
        {editing && (
          <button
            type="button"
            className="tb-btn"
            onClick={() => setEditing(false)}
            title="返回预览"
            aria-label="返回预览"
          >
            返回预览
          </button>
        )}
        {!editing && supportsTextView(kind) && (
          <div className="seg">
            <button
              type="button"
              className={viewMode === "layout" ? "is-active" : ""}
              onClick={() => setViewMode("layout")}
            >
              排版
            </button>
            <button
              type="button"
              className={viewMode === "text" ? "is-active" : ""}
              onClick={() => setViewMode("text")}
            >
              文本
            </button>
          </div>
        )}
        {!editing && supportsZoom(effectiveKind) && (
          <div className="seg" role="group" aria-label="预览缩放">
            <button
              type="button"
              onClick={() => setZoom((z) => Math.max(ZOOM_MIN, z - 10))}
              disabled={zoom <= ZOOM_MIN}
              title="缩小"
              aria-label="缩小预览"
            >
              −
            </button>
            <span className="seg-val">{zoom}%</span>
            <button
              type="button"
              onClick={() => setZoom((z) => Math.min(ZOOM_MAX, z + 10))}
              disabled={zoom >= ZOOM_MAX}
              title="放大"
              aria-label="放大预览"
            >
              ＋
            </button>
            {/* 重置按钮只在偏离 100% 时出现：常驻的话，默认缩放下工具条
                里会并排出现两个 "100%"（当前值 + 写死的重置值），看着像重复。 */}
            {zoom !== 100 && (
              <button
                type="button"
                className="seg-reset"
                onClick={() => setZoom(100)}
                title="重置为 100%"
                aria-label="重置预览缩放"
              >
                <RotateCcw className="icon-sm" />
              </button>
            )}
          </div>
        )}
      </div>
      <div className="preview-pane-inner">
        {editing ? (
          <Suspense fallback={<div className="preview-state-center">正在准备编辑器…</div>}>
            <ImageEditor
              path={path}
              name={name}
              source={source}
              onClose={() => setEditing(false)}
            />
          </Suspense>
        ) : readyState ? (
          <PreviewSurface
            payload={readyState.payload}
            source={source}
            isDark={isDark}
            fileName={name}
            resolvedPath={readyState.resolvedPath}
            zoom={zoom}
            onOpenExternalUrl={async (target) => {
              // 优先 `openWithSystem` —— desktop 走 Tauri 浏览器命令，
              // viewer 走 explorer（html 会拉起默认浏览器）。失败时再回
              // 到 `window.open`，让浏览器的默认行为兜底。
              if (source.openWithSystem) {
                const r = await source.openWithSystem(target);
                if (r.ok) return;
              }
              if (typeof window !== "undefined") {
                try {
                  window.open(target, "_blank", "noopener,noreferrer");
                } catch {
                  /* 沙箱阻挡时无可作为 */
                }
              }
            }}
          />
        ) : state.status === "error" ? (
          <div className="preview-state-center">
            <div className="preview-state-error">
              <p>{state.message}</p>
              <small>{path}</small>
            </div>
          </div>
        ) : (
          <div className="preview-state-center">正在读取 {name}…</div>
        )}
      </div>
    </div>
  );
}
