// 钉住的预览浮窗（重量模式）。
//
// **只读**——这是硬约束：主窗口已经在编辑同一个文件了，浮窗再来一份可
// 编辑的，就是两个实例写同一个文件（互相覆盖）。浮窗的定位是"随时瞄一眼
// 做对比"，不是第二个编辑器。
//
// **跨窗口状态**：每个 Tauri 窗口是独立 JS realm（模块级变量不共享），
// 所以本组件不依赖主窗口的任何 React 状态——它自己加载、自己渲染。
// 需要跟主窗口说话时，走 Rust 命令或 `emit_to(label)`。

import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  applyViewMode,
  loadPreview,
  PreviewSurface,
  resolveKind,
  type DirEntryLike,
  type PreviewPayload,
} from "@hermix/preview";
import { viewerFileSource } from "./fileSource";
import { Pin, X, ChevronRight, Folder, FileText } from "lucide-react";

interface PinPreset {
  key: string;
  label: string;
}

export function PinWindow({ path }: { path: string }) {
  const [payload, setPayload] = useState<PreviewPayload | null>(null);
  const [resolvedPath, setResolvedPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isDir, setIsDir] = useState(false);
  const [entries, setEntries] = useState<DirEntryLike[]>([]);
  const [top, setTop] = useState(false);
  const [presets, setPresets] = useState<PinPreset[]>([]);
  const [busy, setBusy] = useState(false);

  const name = useMemo(() => path.split(/[\\/]/).pop() ?? path, [path]);

  useEffect(() => {
    void invoke<PinPreset[]>("viewer_pin_presets").then(setPresets).catch(() => {});
  }, []);

  // 文件 → 共享预览层；目录 → 简单列表。
  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        const st = await viewerFileSource.fs?.stat(path);
        if (cancelled) return;
        if (st?.isDir) {
          setIsDir(true);
          const list = await viewerFileSource.fs?.listDir(path, false);
          if (!cancelled) setEntries(list && list.ok ? list.entries : []);
          return;
        }
        const kind = applyViewMode(resolveKind(name), "layout");
        const out = await loadPreview(
          { name, path, workspaceRoot: null, viewMode: "layout", kind },
          viewerFileSource,
        );
        if (cancelled) return;
        if (out.ok) {
          setPayload(out.payload);
          setResolvedPath(out.resolvedPath);
        } else {
          setError(out.message);
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [path, name]);

  const toggleTop = useCallback(async () => {
    setBusy(true);
    try {
      const label = getCurrentLabel();
      await invoke("viewer_pin_set_top", { label, top: !top });
      setTop(!top);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [top]);

  const moveTo = useCallback(async (preset: string) => {
    setBusy(true);
    try {
      await invoke("viewer_pin_move_preset", { label: getCurrentLabel(), preset });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const close = useCallback(async () => {
    try {
      await invoke("viewer_pin_close", { label: getCurrentLabel() });
    } catch {
      window.close();
    }
  }, []);

  return (
    <div className="pin-root">
      <div className="pin-bar" data-tauri-drag-region>
        <Pin className="icon-sm" />
        <span className="pin-title" title={path}>
          {name}
        </span>
        <span className="pin-path">{path}</span>
        <div className="toolbar-spacer" />
        <button
          type="button"
          className={top ? "tb-btn is-active" : "tb-btn"}
          onClick={() => void toggleTop()}
          disabled={busy}
          title="置顶（浮在其它窗口之上）"
          aria-label="切换置顶"
        >
          置顶
        </button>
        <select
          className="pin-preset"
          defaultValue=""
          disabled={busy}
          aria-label="移动到屏幕位置"
          onChange={(e) => {
            if (e.target.value) void moveTo(e.target.value);
            e.target.value = "";
          }}
        >
          <option value="">移到…</option>
          {presets.map((p) => (
            <option key={p.key} value={p.key}>
              {p.label}
            </option>
          ))}
        </select>
        <button
          type="button"
          className="tb-btn"
          onClick={() => void close()}
          title="关闭浮窗"
          aria-label="关闭浮窗"
        >
          <X className="icon-sm" />
        </button>
      </div>

      <div className="pin-body">
        {error ? (
          <div className="preview-state-center">
            <div className="preview-state-error">
              <p>{error}</p>
              <small>{path}</small>
            </div>
          </div>
        ) : isDir ? (
          <ul className="pin-list">
            {entries.map((e) => (
              <li key={e.path ?? e.name}>
                <span className="pin-entry-name">
                  {e.isDir ? <Folder className="icon-sm" /> : <FileText className="icon-sm" />}
                  {e.name}
                </span>
                {e.isDir && (
                  <button
                    type="button"
                    className="tb-btn"
                    title="把这个子目录钉成新浮窗"
                    aria-label={`钉住 ${e.name}`}
                    onClick={() => void invoke("viewer_pin_open", { path: e.path })}
                  >
                    <ChevronRight className="icon-sm" />
                  </button>
                )}
              </li>
            ))}
            {entries.length === 0 && <li className="pin-empty">空目录</li>}
          </ul>
        ) : payload ? (
          <PreviewSurface
            payload={payload}
            source={viewerFileSource}
            isDark={document.documentElement.classList.contains("dark")}
            fileName={name}
            resolvedPath={resolvedPath}
            zoom={100}
          />
        ) : (
          <div className="preview-state-center">正在读取 {name}…</div>
        )}
      </div>
    </div>
  );
}

/** 当前窗口的 label（Rust 侧按 label 认领浮窗）。 */
function getCurrentLabel(): string {
  try {
    // @tauri-apps/api 的 window API 在这里可用；拿不到就退回标签前缀。
    const meta = (window as unknown as { __TAURI_INTERNALS__?: { metadata?: { currentWindow?: { label?: string } } } })
      .__TAURI_INTERNALS__?.metadata?.currentWindow?.label;
    return meta ?? "pin-0";
  } catch {
    return "pin-0";
  }
}
