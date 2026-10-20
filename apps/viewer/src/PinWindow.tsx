// 钉住浮窗的**轻量入口**：只渲染被钉住的那个预览，不加载资源管理器 /
// AI 栏 / 工具条。
//
// 浮窗的身份来自**窗口 label**（`pin-` 开头）—— 不走 URL 参数：Tauri 的
// 资源服务对带 query 的地址找不到前端资源 → 整窗空白（实测）。
// **路径**由 Rust 侧按 label 查（`pin_path`）：Windows 路径带盘符与反斜杠，
// 塞进 URL 要编码又要解码。

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
import { currentWindowLabel } from "./windowRole";

/** 目录条目（共享层的 mtime 可能是 null）。 */
interface DirEntry {
  name: string;
  path: string;
  isDir: boolean;
  size: number;
  mtime: string | null;
}

export function PinWindow() {
  const label = useMemo(() => currentWindowLabel(), []);
  const [path, setPath] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [isDir, setIsDir] = useState(false);
  const [entries, setEntries] = useState<DirEntry[]>([]);
  const [preview, setPreview] = useState<Extract<LoadOutcome, { ok: true }> | null>(null);

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

  if (error) {
    return <div className="pin-empty">{error}</div>;
  }
  if (!path) {
    return <div className="pin-empty">载入中…</div>;
  }
  if (isDir) {
    return (
      <div className="pin-dir">
        <div className="pin-dir-title" title={path}>
          {path.split(/[\\/]/).pop()}
        </div>
        <ul className="pin-dir-list">
          {entries.map((e) => (
            <li key={e.path} className="pin-dir-item">
              <span className={e.isDir ? "is-dir" : ""}>{e.name}</span>
              <span className="pin-dir-size">{e.isDir ? "—" : formatSize(e.size)}</span>
            </li>
          ))}
          {entries.length === 0 && <li className="pin-dir-item pin-dir-empty">（空目录）</li>}
        </ul>
      </div>
    );
  }
  if (!preview) {
    return <div className="pin-empty">载入中…</div>;
  }
  return (
    <div className="pin-file">
      <PreviewSurface
        payload={preview.payload}
        source={viewerFileSource}
        isDark={document.documentElement.dataset.theme === "dark"}
        fileName={path.split(/[\\/]/).pop() ?? path}
        resolvedPath={preview.resolvedPath}
        zoom={100}
      />
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
