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
import { isMediaFile, MediaView } from "./media/MediaView";

// 编辑器懒加载：它们是重量模式的重量功能，纯净模式与文本预览都不该
// 为它们付首屏 bundle 的钱。
const ImageEditor = lazy(() =>
  import("./image/ImageEditor").then((m) => ({ default: m.ImageEditor })),
);
const PdfPageEditor = lazy(() =>
  import("./document/PdfPageEditor").then((m) => ({ default: m.PdfPageEditor })),
);
const TextEditor = lazy(() =>
  import("./document/TextEditor").then((m) => ({ default: m.TextEditor })),
);

const ZOOM_MIN = 50;
const ZOOM_MAX = 200;

/** 编辑器种类。文本类（含代码 / Markdown）走文本编辑器。 */
type EditorKind = "none" | "image" | "pdf" | "text";

/** 这个文件有没有对应的编辑器。SVG 是矢量，图片内核解不了 → 不给。 */
function editorFor(kind: PreviewKind, name: string): EditorKind {
  // 媒体走 asset 协议播放，不进文本编辑器。
  if (isMediaFile(name)) return "none";
  if (kind === "image") return isEditableImage(name) ? "image" : "none";
  if (kind === "pdf") return "pdf";
  if (kind === "text" || kind === "code" || kind === "markdown") return "text";
  return "none";
}

/** 编辑按钮的提示文案（同时用作 aria-label）。 */
const EDITOR_HINT: Record<Exclude<EditorKind, "none">, string> = {
  image: "编辑图片（裁剪 / 调整 / 转换格式）",
  pdf: "编辑 PDF（删页 / 排序 / 旋转 / 插空白页）",
  text: "编辑文本（另存为新文件）",
};

/**
 * 「打不开」时的落点：不是一句红字，而是**元信息卡**。
 *
 * 用户遇到看不懂的类型时真正需要的是：它多大、什么时候改的、用什么
 * 程序能打开。所以这里把 stat 拿到的元信息摆出来，并给两个能立刻
 * 采取行动的按钮。
 */
function UnsupportedCard({
  message,
  path,
  name,
  source,
}: {
  message: string;
  path: string;
  name: string;
  source: FileSource;
}) {
  const [meta, setMeta] = useState<{ size: number; mtime: string | null } | null>(null);
  useEffect(() => {
    let cancelled = false;
    void source.fs?.stat(path).then((st) => {
      if (cancelled || !st) return;
      setMeta({ size: st.size, mtime: st.mtime });
    });
    return () => {
      cancelled = true;
    };
  }, [path, source]);

  return (
    <div className="preview-state-center">
      <div className="meta-card">
        <p className="meta-card-msg">{message}</p>
        <dl className="meta-card-info">
          <div>
            <dt>名称</dt>
            <dd title={path}>{name}</dd>
          </div>
          <div>
            <dt>大小</dt>
            <dd>{meta ? formatSize(meta.size) : "—"}</dd>
          </div>
          <div>
            <dt>修改时间</dt>
            <dd>{meta?.mtime ? formatTime(meta.mtime) : "—"}</dd>
          </div>
        </dl>
        <div className="meta-card-actions">
          <button
            type="button"
            className="tb-btn"
            onClick={() => void source.openWithSystem?.(path)}
            disabled={!source.openWithSystem}
          >
            用系统程序打开
          </button>
          <button
            type="button"
            className="tb-btn"
            onClick={() => void source.revealInExplorer?.(path)}
            disabled={!source.revealInExplorer}
          >
            在资源管理器中显示
          </button>
        </div>
      </div>
    </div>
  );
}

function formatSize(n: number): string {
  if (!n) return "0 B";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

function formatTime(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}/${pad(d.getMonth() + 1)}/${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

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
  const [editor, setEditor] = useState<EditorKind>("none");
  const loadToken = useRef(0);

  const kind: PreviewKind = useMemo(() => resolveKind(name), [name]);
  const effectiveKind = applyViewMode(kind, viewMode);
  /** 媒体走 asset 协议，不进共享预览管线（几百 MB 的文件不经 IPC 搬）。 */
  const isMedia = useMemo(() => isMediaFile(name), [name]);

  // 加载阶段：每次 path / viewMode 变了重读
  useEffect(() => {
    if (isMedia) return;
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
  }, [name, path, viewMode, effectiveKind, source, isMedia]);

  // 外壳（工具条 + 内容区）常驻：载入 / 错误态也走同一套壳，
  // 否则状态提示会掉到 .preview-body 左边（flex 行容器里宽度自适应），
  // 表现为"贴在左上角而不是整栏居中"。
  const readyState = state.status === "ready" ? state : null;
  // 编辑器按**原始 kind** 判定：PDF 在「文本」视图下会被转成 markdown
  // 分支（applyViewMode），那时不该再给页编辑器。
  const editorKind = canEdit ? editorFor(kind, name) : "none";
  const editing = editor !== "none";

  return (
    <div className="preview-pane">
      <div className="preview-toolbar">
        <span className="preview-toolbar-name" title={path}>{name}</span>
        <span className="preview-toolbar-spacer" />
        {editorKind !== "none" && !editing && (
          <button
            type="button"
            className="tb-btn"
            onClick={() => setEditor(editorKind)}
            title={EDITOR_HINT[editorKind]}
            aria-label={EDITOR_HINT[editorKind]}
          >
            <Pencil className="icon-sm" />
            编辑
          </button>
        )}
        {editing && (
          <button
            type="button"
            className="tb-btn"
            onClick={() => setEditor("none")}
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
        {isMedia ? (
          <MediaView
            path={path}
            name={name}
            onFallback={() => void source.openWithSystem?.(path)}
          />
        ) : editor === "image" ? (
          <Suspense fallback={<div className="preview-state-center">正在准备编辑器…</div>}>
            <ImageEditor
              path={path}
              name={name}
              source={source}
              onClose={() => setEditor("none")}
            />
          </Suspense>
        ) : editor === "pdf" ? (
          <Suspense fallback={<div className="preview-state-center">正在准备编辑器…</div>}>
            <PdfPageEditor
              path={path}
              name={name}
              source={source}
              onClose={() => setEditor("none")}
            />
          </Suspense>
        ) : editor === "text" ? (
          <Suspense fallback={<div className="preview-state-center">正在准备编辑器…</div>}>
            <TextEditor
              path={path}
              name={name}
              source={source}
              onClose={() => setEditor("none")}
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
          <UnsupportedCard message={state.message} path={path} name={name} source={source} />
        ) : (
          <div className="preview-state-center">正在读取 {name}…</div>
        )}
      </div>
    </div>
  );
}
