// 预览宿主：在 viewer 中用共享预览管线（@hermix/preview）渲染文件。
// 一份 PreviewHost = 一个 tab 的正文。父组件负责调度 source 读、
// 失败重试、tab 关闭。
//
// 渲染走共享包，与桌面 app 完全一致：viewer / desktop 用同一份解析器，
// 共享一份样式 token。

import { useEffect, useMemo, useRef, useState } from "react";
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

export function PreviewHost({
  path,
  name,
  source,
  isDark,
}: {
  path: string;
  name: string;
  source: FileSource;
  isDark: boolean;
}) {
  const [state, setState] = useState<PreviewState>({ status: "loading", kind: "text" });
  const [viewMode, setViewMode] = useState<ViewMode>("layout");
  const [zoom, setZoom] = useState(100);
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

  if (state.status === "loading" || state.status === "idle") {
    return <div className="preview-state-center">正在读取 {name}…</div>;
  }
  if (state.status === "error") {
    return (
      <div className="preview-state-error">
        <p>{state.message}</p>
        <small>{path}</small>
      </div>
    );
  }
  // state.status === "ready"
  const readyState = state;

  return (
    <div className="preview-pane h-full min-h-0 overflow-hidden flex flex-col">
      <div className="preview-toolbar">
        <span className="preview-toolbar-name" title={path}>{name}</span>
        <span className="preview-toolbar-spacer" />
        {supportsTextView(kind) && (
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
        {supportsZoom(effectiveKind) && (
          <div className="seg">
            <button type="button" onClick={() => setZoom((z) => Math.max(50, z - 10))}>−</button>
            <span className="seg-val">{zoom}%</span>
            <button type="button" onClick={() => setZoom((z) => Math.min(200, z + 10))}>+</button>
            <button type="button" onClick={() => setZoom(100)}>100%</button>
          </div>
        )}
      </div>
      <div className="preview-pane-inner flex-1 overflow-auto min-h-0">
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
      </div>
    </div>
  );
}
