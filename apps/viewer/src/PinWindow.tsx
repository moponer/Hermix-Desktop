// 钉住浮窗的**轻量入口**：只渲染被钉住的那个预览，不加载资源管理器 /
// AI 栏 / 工具条。
//
// 浮窗的身份来自**窗口 label**（`pin-` 开头）—— 不走 URL 参数：Tauri 的
// 资源服务对带 query 的地址找不到前端资源 → 整窗空白（实测）。
// **路径**由 Rust 侧按 label 查（`pin_path`）：Windows 路径带盘符与反斜杠，
// 塞进 URL 要编码又要解码。
//
// 结构约定（外壳 → 内容）：
//   .pin-root   整窗的 flex 列
//     .pin-bar  顶部一条：文件名 + 路径 + 摆位按钮（贴左 / 贴右 / 居中）
//     .pin-body 内容区
//       .preview-pane  ← **共享渲染层的作用域**
//
// `.preview-pane` 这一层不能少：共享样式（Markdown 的目录列、表格的纸面
// 样式、PDF/PPTX 的画布居中）全部写成 `.preview-pane .xxx` 的后代选择器，
// 没有这个祖先就等于没有样式 —— 表现为"浮窗里的 md 目录排版全乱"
// （目录与正文挤成一列、章节跑到最上面）。

import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  applyViewMode,
  loadPreview,
  PreviewSurface,
  resolveKind,
  type LoadOutcome,
  type PreviewKind,
} from "@hermix/preview";
import { viewerFileSource } from "./fileSource";
import { isDarkResolved, type ResolvedTheme } from "./theme";
import { currentWindowLabel } from "./windowRole";

/** 目录条目（共享层的 mtime 可能是 null）。 */
interface DirEntry {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  mtime: string | null;
}

/** 摆位预设（对应 Rust 侧 pin_place 的 preset 参数）。 */
const PLACEMENTS: { id: string; label: string; hint: string }[] = [
  { id: "left", label: "贴左", hint: "把浮窗贴到屏幕左边缘" },
  { id: "right", label: "贴右", hint: "把浮窗贴到屏幕右边缘" },
  { id: "center", label: "居中", hint: "把浮窗放到屏幕中间" },
];

export function PinWindow() {
  const label = useMemo(() => currentWindowLabel(), []);
  const [path, setPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isDir, setIsDir] = useState(false);
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [preview, setPreview] = useState<Extract<LoadOutcome, { ok: true }> | null>(null);
  const [placeError, setPlaceError] = useState<string | null>(null);

  // 1. 问 Rust 要被钉住的路径。
  useEffect(() => {
    let cancelled = false;
    void invoke<string | null>("pin_path", { label })
      .then((p) => {
        if (cancelled) return;
        setPath(p);
        if (!p) setError("这个浮窗没有绑定路径");
      })
      .catch(() => {
        if (!cancelled) setError("读不到被钉住的路径");
      });
    return () => {
      cancelled = true;
    };
  }, [label]);

  // 2. 目录 → 条目列表；文件 → 共享预览层（与主窗口同一套渲染）。
  useEffect(() => {
    if (!path) return;
    let cancelled = false;
    void (async () => {
      const st = await viewerFileSource.fs?.stat(path);
      if (cancelled) return;
      if (st?.isDir) {
        setIsDir(true);
        const list = await viewerFileSource.fs?.listDir(path, false);
        if (cancelled) return;
        if (list?.ok) {
          setEntries(
            list.entries.map((e) => ({
              name: e.name,
              path: e.path,
              isDir: e.isDir,
              size: e.size,
              mtime: e.mtime,
            })),
          );
        }
        return;
      }
      setIsDir(false);
      const name = path.split(/[\\/]/).pop() ?? path;
      const kind: PreviewKind = resolveKind(name);
      const out = await loadPreview(
        { name, path, workspaceRoot: null, viewMode: "layout", kind: applyViewMode(kind, "layout") },
        viewerFileSource,
      );
      if (cancelled) return;
      if (!out.ok) {
        setError(out.message);
        return;
      }
      setPreview(out);
    })();
    return () => {
      cancelled = true;
    };
  }, [path]);

  const place = (preset: string) => {
    setPlaceError(null);
    void invoke("pin_place", { label, preset }).catch((e) => {
      setPlaceError(String(e instanceof Error ? e.message : e));
    });
  };

  const fileName = path ? path.split(/[\\/]/).pop() ?? path : "";
  const dirName = path ? path.split(/[\\/]/).slice(0, -1).join("\\") : "";

  return (
    <div className="pin-root">
      <div className="pin-bar">
        <span className="pin-title" title={path ?? ""}>
          {fileName || "…"}
        </span>
        <span className="pin-path" title={path ?? ""}>
          {dirName}
        </span>
        {!isDir && (
          <>
            {PLACEMENTS.map((p) => (
              <button
                key={p.id}
                type="button"
                className="pin-preset"
                onClick={() => place(p.id)}
                title={p.hint}
                aria-label={p.hint}
              >
                {p.label}
              </button>
            ))}
          </>
        )}
        {placeError && <span className="tb-error">{placeError}</span>}
      </div>

      <div className="pin-body">
        {error ? (
          <div className="pin-empty">{error}</div>
        ) : !path ? (
          <div className="pin-empty">载入中…</div>
        ) : isDir ? (
          <ul className="pin-list">
            {entries.map((e) => (
              <li key={e.path}>
                <span className="pin-entry-name">
                  <span className={e.isDir ? "is-dir" : ""}>{e.name}</span>
                </span>
                <span className="pin-dir-size">{e.isDir ? "—" : formatSize(e.size)}</span>
              </li>
            ))}
            {entries.length === 0 && <li className="pin-empty">（空目录）</li>}
          </ul>
        ) : !preview ? (
          <div className="pin-empty">载入中…</div>
        ) : (
          <div className="preview-pane">
            <PreviewSurface
              payload={preview.payload}
              source={viewerFileSource}
              isDark={isDarkResolved((document.documentElement.dataset.theme ?? "light") as ResolvedTheme)}
              fileName={fileName}
              resolvedPath={preview.resolvedPath}
              zoom={100}
            />
          </div>
        )}
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
