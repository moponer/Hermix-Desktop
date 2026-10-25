// 预览宿主：在 viewer 中用共享预览管线（@hermix/preview）渲染文件。
// 一份 PreviewHost = 一个 tab 的正文。父组件负责调度 source 读、
// 失败重试、tab 关闭。
//
// 渲染走共享包，与桌面 app 完全一致：viewer / desktop 用同一份解析器，
// 共享一份样式 token。

import { useEffect, useMemo, useRef, useState } from "react";
import { lazy, Suspense } from "react";
import { createPortal } from "react-dom";
import { Pencil, Pin, Play, RotateCcw } from "lucide-react";
import {
  loadPreview,
  PreviewSurface,
  resolveKind,
  type FileSource,
  type LoadOutcome,
  type PreviewState,
  supportsZoom,
  type PreviewKind,
  type ViewMode,
} from "@hermix/preview";
import { isMediaFile, MediaView } from "./media/MediaView";
import { EDITOR_HINT, editorFor, type EditorKind } from "./editKind";
import { pinPreview } from "./pin";

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
const DocxEditor = lazy(() =>
  import("./document/DocxEditor").then((m) => ({ default: m.DocxEditor })),
);
const SheetEditor = lazy(() =>
  import("./document/SheetEditor").then((m) => ({ default: m.SheetEditor })),
);
const PptxEditor = lazy(() =>
  import("./document/PptxEditor").then((m) => ({ default: m.PptxEditor })),
);
const ZOOM_MIN = 50;
const ZOOM_MAX = 200;

/**
 * 「打不开」时的落点：不是一句红字，而是**元信息卡**。
 *
 * 用户遇到看不懂的类型时真正需要的是：它多大、什么时候改的、用什么
 * 程序能打开。所以这里把 stat 拿到的元信息摆出来，并给两个能立刻
 * 采取行动的按钮。
 *
 * 可执行文件（exe / msi / …）多给一个「立即运行」——它不可预览，
 * 但用户要的往往正是把它跑起来。执行前由后端弹原生确认框。
 */
function UnsupportedCard({
  message,
  path,
  name,
  source,
  executable = false,
}: {
  message: string;
  path: string;
  name: string;
  source: FileSource;
  executable?: boolean;
}) {
  const [meta, setMeta] = useState<{ size: number; mtime: string | null } | null>(null);
  const [error, setError] = useState<string | null>(null);
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

  const run = async () => {
    setError(null);
    const r = await source.runExecutable?.(path);
    if (r && !r.ok) setError(r.message ?? "运行失败");
  };

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
        {executable && (
          <button
            type="button"
            className="tb-btn meta-card-run"
            onClick={() => void run()}
            disabled={!source.runExecutable}
            title="运行这个可执行文件（执行前会弹出确认）"
          >
            <Play size={14} className="icon-sm" />
            立即运行
          </button>
        )}
        {error && <p className="meta-card-error">{error}</p>}
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
  editRequest = 0,
  showName = true,
  actionsSlot = null,
}: {
  path: string;
  name: string;
  source: FileSource;
  isDark: boolean;
  /** 重量模式才给编辑能力（纯净模式是"双击就看"，不塞工具）。 */
  canEdit?: boolean;
  /**
   * 外部要求打开编辑器（右键菜单 / 菜单栏 / 快捷键）。用递增计数而不是
   * 布尔量：布尔量「已经请求过了」就再也触发不了第二次。
   */
  editRequest?: number;
  /**
   * 工具条里是否显示文件名。纯净模式下顶栏地址栏已经显示，这里再写一遍
   * 就是同一串文件名隔 30px 出现两次。
   */
  showName?: boolean;
  /**
   * 标签条右端的动作区（heavy 模式由 App 传入）。工具按钮（编辑 / 钉住 /
   * 视图切换）经 portal 投到这里——它们原先占独立一行，且行内的文件名
   * 与标签条重复；并入标签行后预览区顶部少一行。
   * showName=false（纯净模式）时不用 portal，走自带工具行。
   */
  actionsSlot?: HTMLElement | null;
}) {
  const [state, setState] = useState<PreviewState>({ status: "loading", kind: "text" });
  const [zoom, setZoom] = useState(100);
  const [editor, setEditor] = useState<EditorKind>("none");
  /** 钉住浮窗失败的原因（工具条里显示，别让「点了没反应」变成谜）。 */
  const [pinError, setPinError] = useState<string | null>(null);
  const loadToken = useRef(0);

  const kind: PreviewKind = useMemo(() => resolveKind(name), [name]);
  /** 「排版 / 文本」切换已收：恒排版视图（用户拍板——打开默认就是预览，
   *  文本模式没有使用价值），viewMode 状态随之删除。 */
  const viewMode: ViewMode = "layout";
  const effectiveKind = kind;
  /** 媒体走 asset 协议，不进共享预览管线（几百 MB 的文件不经 IPC 搬）。 */
  const isMedia = useMemo(() => isMediaFile(name), [name]);

  /**
   * Office 文档的**编辑 / 预览**切换。
   *
   * docx / xlsx / xlsm 在重量模式下**默认就是可编辑的**（用户诉求：
   * "打开 word 文档、excel 默认直接就可以编辑"），不套一层「编辑模式」——
   * Office 软件本来就是这样。纯净模式（轻量）不给编辑能力，仍是只读预览。
   *
   * 这里取代了原来的「排版 / 文本」开关：那两个词对 Office 文档没有意义
   * （点「文本」什么都不会变，applyViewMode 对 docx/pptx 原样返回），
   * 用户真正要的就是「能改 / 只看」这一个选择。
   */
  const officeKind = useMemo<"docx" | "sheet" | "pptx" | null>(() => {
    if (!canEdit) return null;
    if (kind === "docx") return "docx";
    if (kind === "pptx") return "pptx";
    if (kind === "spreadsheet") {
      const ext = name.split(".").pop()?.toLowerCase() ?? "";
      // xls 是 BIFF 二进制，没有可靠的写入器 → 只读预览。
      return ext === "xlsx" || ext === "xlsm" ? "sheet" : null;
    }
    return null;
  }, [canEdit, kind, name]);
  /** Office 文档当前是「编辑」还是「预览」。默认编辑 —— 换了文件也回编辑。 */
  const [officeMode, setOfficeMode] = useState<"edit" | "preview">("edit");
  useEffect(() => {
    setOfficeMode("edit");
  }, [path]);

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
        setState({ status: "error", message: out.message, executable: out.executable });
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
  /** Office 就地编辑生效中（docx / xlsx，且用户没切到只读预览）。 */
  const officeEditing = officeKind !== null && officeMode === "edit";

  // 外部（右键菜单 / 菜单栏 / 快捷键）要求打开编辑器。
  useEffect(() => {
    if (!editRequest || editorKind === "none") return;
    setEditor(editorKind);
    // 只在请求号变化时响应：editorKind 变化不该把人拽进编辑器。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [editRequest]);

  /** 工具区控件（编辑 / 钉住 / 视图切换 / 缩放）。heavy 模式经 portal
   *  投进标签条右端动作区（文件名标签上已有，独立一行只重复它）；
   *  纯净模式没有标签条，走自带工具行。 */
  const controls = (
    <>
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
      {!editing && (
        <button
          type="button"
          className="tb-btn"
          onClick={() => {
            void pinPreview(path).then((r) => {
              setPinError("error" in r ? r.error : null);
            });
          }}
          title="钉住到独立浮窗（可挪到屏幕任意位置，方便对比）"
          aria-label="钉住到独立浮窗"
        >
          <Pin className="icon-sm" />
          钉住
        </button>
      )}
      {pinError && <span className="tb-error">{pinError}</span>}
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
      {/* Word / Excel：编辑 ↔ 预览。原先这里是「排版 / 文本」，对 Office
          文档点了什么都不会变（applyViewMode 对 docx/pptx 原样返回）。 */}
      {officeKind && (
        <div className="seg" role="group" aria-label="编辑与预览">
          <button
            type="button"
            className={officeMode === "edit" ? "is-active" : ""}
            onClick={() => setOfficeMode("edit")}
            aria-pressed={officeMode === "edit"}
          >
            编辑
          </button>
          <button
            type="button"
            className={officeMode === "preview" ? "is-active" : ""}
            onClick={() => setOfficeMode("preview")}
            aria-pressed={officeMode === "preview"}
          >
            预览
          </button>
        </div>
      )}
      {/* 「排版 / 文本」切换已收（2026-10-25 用户拍板）：打开默认就是排版
          预览，文本模式（markitdown 抽取）没有使用价值。viewMode 恒
          layout，applyViewMode 不再参与调度。 */}
      {!editing && !officeEditing && supportsZoom(effectiveKind) && (
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
    </>
  );

  return (
    <div className="preview-pane">
      {/* 预览工具条常驻：Office 的「编辑 / 预览」切换、缩放、钉住都在这里。
          Office 编辑器自己那条工具条只管导出 / 另存为。 */}
      {/* 工具按钮：heavy 经 portal 进标签条动作区（actionsSlot 已在
          App mount 完成后传入，lazy 的本组件首渲即有值，不会闪空）；
          纯净模式无标签条，走自带工具行并显示文件名。 */}
      {showName && actionsSlot ? (
        createPortal(<div className="tabbar-actions-inner">{controls}</div>, actionsSlot)
      ) : (
        <div className="preview-toolbar">
          {/* 编辑态下文件名由编辑器那条工具条显示，这里不再重复一遍。 */}
          {!officeEditing && (
            <span className="preview-toolbar-name" title={path}>
              {name}
            </span>
          )}
          <span className="preview-toolbar-spacer" />
          {controls}
        </div>
      )}
      <div className="preview-pane-inner">
        {isMedia ? (
          <MediaView
            path={path}
            name={name}
            onFallback={() => void source.openWithSystem?.(path)}
          />
        ) : officeEditing && officeKind === "docx" ? (
          <Suspense fallback={<div className="preview-state-center">正在准备编辑器…</div>}>
            <DocxEditor
              path={path}
              name={name}
              onPreview={() => setOfficeMode("preview")}
            />
          </Suspense>
        ) : officeEditing && officeKind === "sheet" ? (
          <Suspense fallback={<div className="preview-state-center">正在准备编辑器…</div>}>
            <SheetEditor
              path={path}
              name={name}
              source={source}
              onPreview={() => setOfficeMode("preview")}
            />
          </Suspense>
        ) : officeEditing && officeKind === "pptx" ? (
          <Suspense fallback={<div className="preview-state-center">正在准备编辑器…</div>}>
            <PptxEditor
              path={path}
              name={name}
              source={source}
              onPreview={() => setOfficeMode("preview")}
            />
          </Suspense>
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
          <UnsupportedCard
            message={state.message}
            path={path}
            name={name}
            source={source}
            executable={state.executable}
          />
        ) : (
          <div className="preview-state-center">正在读取 {name}…</div>
        )}
      </div>
    </div>
  );
}
